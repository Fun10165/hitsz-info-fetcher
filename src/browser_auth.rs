use crate::extract::extract_page_snapshot;
use crate::models::{AuthenticatedFetchResult, EasToken, StudentType};
use anyhow::{Context, Result, anyhow, bail};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::handler::Handler;
use chromiumoxide::cdp::browser_protocol::network::Cookie;
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time::sleep;
use url::Url;

const JW_CAS_URL: &str = "http://jw.hitsz.edu.cn/cas";
const INFO_DEFAULT_URL: &str = "http://info.hitsz.edu.cn/list.jsp?wbtreeid=1053";
const JW_PROFILE_PATH: &str = "/UserManager/queryxsxx";
const DEFAULT_CHROME_EXECUTABLE: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const CHROME_PROFILE_DIRECTORY: &str = "Default";
const EXISTING_BROWSER_WAIT_SECS: u64 = 5;
const REMOTE_DEBUG_CANDIDATES: [&str; 3] = [
    "http://127.0.0.1:9222",
    "http://127.0.0.1:9223",
    "http://localhost:9222",
];

pub async fn login_and_fetch_info_via_browser(
    username: Option<&str>,
    password: Option<&str>,
    info_url: Option<&str>,
) -> Result<AuthenticatedFetchResult> {
    let (mut browser, handler_task, should_close) = open_browser().await?;

    let page = browser
        .new_page(JW_CAS_URL)
        .await
        .context("failed to open JW CAS page in browser")?;

    ensure_logged_in(&page, username, password).await?;

    let jw_landing_url = evaluate_string(&page, "() => window.location.href")
        .await
        .unwrap_or_else(|_| "<unavailable>".to_owned());
    eprintln!("browser login completed; current page is {jw_landing_url}");

    let profile_page = browser
        .new_page("http://jw.hitsz.edu.cn/")
        .await
        .context("failed to open authenticated JW page for profile fetch")?;
    sleep(Duration::from_secs(2)).await;

    let profile_json = evaluate_string_with_retry(
        &profile_page,
        &format!(
            r#"async () => {{
                const profileUrl = new URL({:?}, window.location.origin).toString();
                const resp = await fetch(profileUrl, {{
                    method: 'POST',
                    headers: {{ 'X-Requested-With': 'XMLHttpRequest' }},
                    credentials: 'include'
                }});
                return await resp.text();
            }}"#,
            JW_PROFILE_PATH
        ),
    )
    .await
    .context("failed to fetch authenticated profile JSON through browser session")?;

    if profile_json.contains("session已失效") {
        bail!("browser-authenticated profile request reported expired session");
    }
    if profile_json.trim_start().starts_with('<') {
        bail!("browser-authenticated profile request returned HTML instead of JSON");
    }

    let cookies = browser
        .get_cookies()
        .await
        .context("failed to read browser cookies")?;
    let token = build_browser_token(username, password, &cookies, &profile_json)?;

    let target_url = info_url.unwrap_or(INFO_DEFAULT_URL);
    let target_page = browser
        .new_page(target_url)
        .await
        .with_context(|| format!("failed to open target page {target_url} in browser"))?;
    sleep(Duration::from_secs(3)).await;

    let final_url = evaluate_string_with_retry(&target_page, "() => window.location.href")
        .await
        .context("failed to read final target page URL")?;
    let html = page_content_with_retry(&target_page)
        .await
        .with_context(|| format!("failed to read target page HTML from {target_url}"))?;

    let final_url = Url::parse(&final_url).with_context(|| {
        format!("browser returned invalid final target page URL: {final_url}")
    })?;
    let fetched_page = extract_page_snapshot(&final_url, &html);

    if should_close {
        browser.close().await.ok();
    }
    handler_task.abort();

    Ok(AuthenticatedFetchResult {
        token,
        fetched_page,
    })
}

async fn wait_for_selector(
    page: &chromiumoxide::Page,
    selector: &str,
    timeout: Duration,
) -> Result<()> {
    let started = std::time::Instant::now();
    loop {
        if page.find_element(selector).await.is_ok() {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            bail!("timed out waiting for selector {selector}");
        }
        sleep(Duration::from_millis(250)).await;
    }
}

async fn ensure_logged_in(
    page: &chromiumoxide::Page,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<()> {
    if is_jw_page(page).await?
        || wait_for_existing_session(page, Duration::from_secs(EXISTING_BROWSER_WAIT_SECS)).await?
    {
        return Ok(());
    }

    let username = username.context(
        "no authenticated Chrome session found; pass --username or set HITSZ_USERNAME to fall back to browser login",
    )?;
    let password = password.context(
        "no authenticated Chrome session found; pass --password or set HITSZ_PASSWORD to fall back to browser login",
    )?;

    if wait_for_selector(page, "#password", Duration::from_secs(3)).await.is_err() {
        click_control_by_text(page, "账号登录").await?;
        wait_for_selector(page, "#password", Duration::from_secs(20)).await?;
    }

    fill_credentials(page, username, password).await?;
    click_control_by_text(page, "登录").await?;
    wait_for_login_completion(page).await
}

async fn open_browser() -> Result<(Browser, tokio::task::JoinHandle<Result<()>>, bool)> {
    if let Some((browser, handler)) = try_connect_existing_browser().await? {
        eprintln!("connected to existing Chrome debugging session");
        return Ok((browser, spawn_handler(handler), false));
    }

    if let Some(user_data_dir) = chrome_user_data_dir() {
        if let Some((browser, handler)) = try_launch_browser_with_profile(&user_data_dir).await? {
            eprintln!(
                "launched Chrome with existing user profile at {}",
                user_data_dir.display()
            );
            return Ok((browser, spawn_handler(handler), true));
        }
    }

    let (browser, handler) = launch_browser(None, None)
        .await
        .context("failed to launch Chromium browser")?;
    eprintln!("launched isolated Chrome session; will fall back to explicit login if needed");
    Ok((browser, spawn_handler(handler), true))
}

fn spawn_handler(mut handler: Handler) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        while let Some(event) = handler.next().await {
            if let Err(err) = event {
                eprintln!("chromium handler error: {err}");
                return Err(anyhow!(err));
            }
        }
        Ok(())
    })
}

async fn try_connect_existing_browser() -> Result<Option<(Browser, Handler)>> {
    let mut candidates = Vec::new();
    if let Ok(url) = env::var("HITSZ_CHROME_DEBUG_URL") {
        if !url.trim().is_empty() {
            candidates.push(url);
        }
    }
    for candidate in REMOTE_DEBUG_CANDIDATES {
        candidates.push(candidate.to_owned());
    }

    for candidate in candidates {
        match Browser::connect(candidate.clone()).await {
            Ok(session) => return Ok(Some(session)),
            Err(err) => {
                eprintln!("could not connect to existing Chrome at {candidate}: {err}");
            }
        }
    }
    Ok(None)
}

async fn try_launch_browser_with_profile(
    user_data_dir: &Path,
) -> Result<Option<(Browser, Handler)>> {
    match launch_browser(Some(user_data_dir), Some(chrome_profile_directory())).await {
        Ok(session) => Ok(Some(session)),
        Err(err) => {
            eprintln!(
                "could not launch Chrome with existing user profile {}: {err}",
                user_data_dir.display()
            );
            Ok(None)
        }
    }
}

async fn launch_browser(
    user_data_dir: Option<&Path>,
    profile_directory: Option<String>,
) -> Result<(Browser, Handler)> {
    let mut builder = BrowserConfig::builder();
    builder = builder
        .chrome_executable(chrome_executable())
        .with_head()
        .no_sandbox()
        .window_size(1440, 1000)
        .arg("--disable-gpu")
        .arg("--disable-dev-shm-usage");

    if let Some(user_data_dir) = user_data_dir {
        builder = builder.user_data_dir(user_data_dir);
    }
    if let Some(profile_directory) = profile_directory {
        builder = builder.arg(format!("--profile-directory={profile_directory}"));
    }

    Browser::launch(
        builder
            .build()
            .map_err(|err| anyhow!("failed to build browser config: {err}"))?,
    )
    .await
    .context("failed to launch Chromium browser")
}

fn chrome_user_data_dir() -> Option<PathBuf> {
    if let Ok(path) = env::var("HITSZ_CHROME_USER_DATA_DIR") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }

    let home = env::var("HOME").ok()?;
    let path = PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Google")
        .join("Chrome");
    path.exists().then_some(path)
}

fn chrome_profile_directory() -> String {
    env::var("HITSZ_CHROME_PROFILE_DIRECTORY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| CHROME_PROFILE_DIRECTORY.to_owned())
}

fn chrome_executable() -> String {
    env::var("HITSZ_CHROME_EXECUTABLE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CHROME_EXECUTABLE.to_owned())
}

async fn wait_for_existing_session(page: &chromiumoxide::Page, timeout: Duration) -> Result<bool> {
    let started = std::time::Instant::now();
    loop {
        if is_jw_page(page).await? {
            return Ok(true);
        }
        if started.elapsed() >= timeout {
            return Ok(false);
        }
        sleep(Duration::from_millis(500)).await;
    }
}

async fn wait_for_login_completion(page: &chromiumoxide::Page) -> Result<()> {
    let started = std::time::Instant::now();
    let mut captcha_announced = false;
    let mut two_factor_announced = false;
    let mut two_factor_code_requested = false;
    loop {
        let current_url = match evaluate_string(page, "() => window.location.href").await {
            Ok(url) => url,
            Err(err) if is_missing_context_error(&err) => {
                sleep(Duration::from_millis(500)).await;
                continue;
            }
            Err(err) => return Err(err).context("failed to read current browser URL"),
        };
        let current_html = match page.content().await {
            Ok(html) => html,
            Err(err) if err.to_string().contains("Cannot find context with specified id") => {
                sleep(Duration::from_millis(500)).await;
                continue;
            }
            Err(err) => return Err(err).context("failed to read current browser HTML"),
        };

        if is_jw_url(&current_url) {
            return Ok(());
        }
        if current_html.contains("用户名或密码错误") || current_html.contains("账号或密码错误") {
            bail!("browser login page reports invalid username or password");
        }
        if current_html.contains("两步验证")
            || current_html.contains("二次验证")
            || current_html.contains("双因素")
            || current_html.contains("动态码")
        {
            if !two_factor_announced {
                let artifact_dir = persist_page_debug_artifacts(
                    page,
                    "two-factor",
                    &current_url,
                    Some(&current_html),
                )
                .await?;
                eprintln!(
                    "two-factor page detected; debug artifacts saved to {}",
                    artifact_dir.display()
                );
                eprintln!(
                    "waiting in visible browser window for HIT multifactor completion"
                );
                two_factor_announced = true;
            }
            if !two_factor_code_requested {
                if request_two_factor_code(page).await? {
                    eprintln!("requested HIT app verification code from multifactor page");
                }
                two_factor_code_requested = true;
            }
        }
        if current_html.contains("验证码") && current_html.contains("captcha") {
            if !captcha_announced {
                eprintln!(
                    "captcha detected in browser flow; waiting for completion in visible browser window"
                );
                captcha_announced = true;
            }
        }
        if started.elapsed() >= Duration::from_secs(300) {
            let artifact_dir = persist_page_debug_artifacts(
                page,
                "login-timeout",
                &current_url,
                Some(&current_html),
            )
            .await?;
            bail!(
                "timed out waiting for browser login to redirect away from IDS login page; debug artifacts saved to {}",
                artifact_dir.display()
            );
        }
        sleep(Duration::from_millis(500)).await;
    }
}

async fn request_two_factor_code(page: &chromiumoxide::Page) -> Result<bool> {
    let value = page
        .evaluate_function(
            r#"() => {
                const button = document.querySelector('#getDynamicCode');
                if (!button || button.disabled) {
                    return false;
                }
                button.click();
                return true;
            }"#,
        )
        .await
        .context("failed to request multifactor verification code")?;
    value
        .into_value::<bool>()
        .context("multifactor code request result was not a boolean")
}

async fn is_jw_page(page: &chromiumoxide::Page) -> Result<bool> {
    let current_url = evaluate_string_with_retry(page, "() => window.location.href").await?;
    Ok(is_jw_url(&current_url))
}

fn is_jw_url(current_url: &str) -> bool {
    Url::parse(current_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .as_deref()
        == Some("jw.hitsz.edu.cn")
}

async fn evaluate_string(page: &chromiumoxide::Page, js: &str) -> Result<String> {
    let value = page
        .evaluate_function(js)
        .await
        .context("page JavaScript function evaluation failed")?;
    value
        .into_value::<String>()
        .context("page JavaScript evaluation did not return a string")
}

async fn evaluate_string_with_retry(page: &chromiumoxide::Page, js: &str) -> Result<String> {
    let started = std::time::Instant::now();
    loop {
        match evaluate_string(page, js).await {
            Ok(value) => return Ok(value),
            Err(err) if is_missing_context_error(&err) && started.elapsed() < Duration::from_secs(10) => {
                sleep(Duration::from_millis(500)).await;
                continue;
            }
            Err(err) => return Err(err),
        }
    }
}

async fn page_content_with_retry(page: &chromiumoxide::Page) -> Result<String> {
    let started = std::time::Instant::now();
    loop {
        match page.content().await {
            Ok(html) => return Ok(html),
            Err(err)
                if err.to_string().contains("Cannot find context with specified id")
                    && started.elapsed() < Duration::from_secs(10) =>
            {
                sleep(Duration::from_millis(500)).await;
                continue;
            }
            Err(err) => return Err(anyhow!(err)),
        }
    }
}

async fn click_control_by_text(page: &chromiumoxide::Page, label: &str) -> Result<()> {
    page.evaluate_function(&format!(
        r#"() => {{
            const wanted = {:?};
            const candidates = Array.from(document.querySelectorAll('a,button,input[type="submit"]'));
            const control = candidates.find((node) => ((node.innerText || node.value || '').trim() === wanted));
            if (!control) {{
                throw new Error(`Could not find control: ${{wanted}}`);
            }}
            control.click();
            return wanted;
        }}"#,
        label
    ))
    .await
    .with_context(|| format!("failed to click control with text {label}"))?;
    sleep(Duration::from_secs(1)).await;
    Ok(())
}

async fn fill_credentials(
    page: &chromiumoxide::Page,
    username: &str,
    password: &str,
) -> Result<()> {
    let fill_result = page
        .evaluate_function(&format!(
        r#"() => {{
            const visible = (node) => !!(node && (node.offsetParent !== null || window.getComputedStyle(node).position === 'fixed'));
            const usernameInput = Array.from(document.querySelectorAll('input')).find((node) => node.id === 'username' && visible(node));
            const passwordInput = Array.from(document.querySelectorAll('input')).find((node) => node.id === 'password' && visible(node));
            if (!usernameInput || !passwordInput) {{
                throw new Error('Could not find visible username/password inputs');
            }}
            const applyValue = (node, value) => {{
                node.focus();
                node.value = value;
                node.dispatchEvent(new Event('input', {{ bubbles: true }}));
                node.dispatchEvent(new Event('change', {{ bubbles: true }}));
            }};
            applyValue(usernameInput, {:?});
            applyValue(passwordInput, {:?});
            return {{
                username: usernameInput.value,
                passwordLength: passwordInput.value.length,
            }};
        }}"#,
        username, password
    ))
        .await
        .context("failed to fill username/password in visible browser form")?;
    let fill_result = fill_result
        .into_value::<Value>()
        .context("browser form fill result was not valid JSON")?;
    let filled_username = fill_result
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let password_length = fill_result
        .get("passwordLength")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    eprintln!(
        "filled login form in browser: username_len={}, password_len={}, username_matches={}",
        filled_username.len(),
        password_length,
        filled_username == username
    );
    sleep(Duration::from_millis(500)).await;
    Ok(())
}

async fn persist_page_debug_artifacts(
    page: &chromiumoxide::Page,
    label: &str,
    current_url: &str,
    current_html: Option<&str>,
) -> Result<PathBuf> {
    let title = evaluate_string(page, "() => document.title").await.unwrap_or_default();
    let body_text = evaluate_string(page, "() => document.body ? document.body.innerText : ''")
        .await
        .unwrap_or_default();
    let html = match current_html {
        Some(html) => html.to_owned(),
        None => page.content().await.unwrap_or_default(),
    };

    let mut artifact_dir = std::env::current_dir().context("failed to resolve current directory")?;
    artifact_dir.push("debug-artifacts");
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("failed to create debug artifact directory {}", artifact_dir.display()))?;

    let html_path = artifact_dir.join(format!("{label}.html"));
    let json_path = artifact_dir.join(format!("{label}.json"));
    let metadata = serde_json::json!({
        "label": label,
        "url": current_url,
        "title": title,
        "body_text": body_text,
    });

    fs::write(&html_path, html)
        .with_context(|| format!("failed to write HTML artifact {}", html_path.display()))?;
    fs::write(&json_path, serde_json::to_vec_pretty(&metadata)?)
        .with_context(|| format!("failed to write JSON artifact {}", json_path.display()))?;

    Ok(artifact_dir)
}

fn build_browser_token(
    username: Option<&str>,
    password: Option<&str>,
    cookies: &[Cookie],
    profile_json: &str,
) -> Result<EasToken> {
    let profile: Value = serde_json::from_str(profile_json)
        .with_context(|| format!("failed to parse browser profile JSON: {profile_json}"))?;

    let student_type = match profile.get("PYLX").and_then(Value::as_str) {
        Some("1") => StudentType::Undergrad,
        _ => StudentType::Grad,
    };

    let cookie_map = cookies
        .iter()
        .map(|cookie| (cookie.name.clone(), cookie.value.clone()))
        .collect::<HashMap<_, _>>();

    Ok(EasToken {
        cookies: cookie_map,
        username: username
            .map(str::to_owned)
            .or_else(|| get_string(&profile, "XH"))
            .unwrap_or_default(),
        password: password.unwrap_or_default().to_owned(),
        name: get_string(&profile, "XM"),
        stutype: student_type,
        picture: get_string(&profile, "ZPBSLJ"),
        id: get_string(&profile, "ID"),
        stu_id: get_string(&profile, "XH"),
        school: get_string(&profile, "YXMC"),
        major: get_string(&profile, "ZYMC"),
        grade: get_string(&profile, "NJMC"),
        sfxsx: get_string(&profile, "sfxsx"),
        email: get_string(&profile, "DZYX"),
        phone: get_string(&profile, "LXDH"),
    })
}

fn is_missing_context_error(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.to_string().contains("Cannot find context with specified id"))
}

fn get_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}
