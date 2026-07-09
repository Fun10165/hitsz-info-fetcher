use crate::extract::{extract_notice_items, extract_page_snapshot, find_next_page_url};
use crate::models::{AuthenticatedFetchResult, EasToken, NoticeList, StudentType};
use anyhow::{Context, Result, anyhow, bail};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::handler::Handler;
use chromiumoxide::cdp::browser_protocol::network::Cookie;
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, Write};
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

/// Browser-only login — returns HitToken without fetching info pages.
/// Used by hit_auth module as the browser auth path.
pub async fn browser_login(
    username: &str,
    password: &str,
) -> Result<crate::hit_auth::HitToken> {
    let (mut browser, handler_task, should_close) = open_browser().await?;

    let page = browser
        .new_page(JW_CAS_URL)
        .await
        .context("failed to open JW CAS page in browser")?;

    ensure_logged_in(&page, Some(username), Some(password), true).await?;

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

    // Save cookies for cron reuse
    if let Err(e) = save_cookies_to_file(&cookies, "session-cookies.json") {
        eprintln!("warning: failed to save session cookies: {e}");
    } else {
        eprintln!("session cookies saved to session-cookies.json");
    }

    if should_close {
        browser.close().await.ok();
    }
    handler_task.abort();

    // Build HitToken from profile JSON + cookies
    let token = build_browser_token(Some(username), Some(password), &cookies, &profile_json)?;
    use crate::hit_auth::HitToken;
    Ok(HitToken {
        username: token.username,
        name: token.name,
        student_id: token.stu_id,
        school: token.school,
        stutype: Some(match token.stutype {
            crate::models::StudentType::Undergrad => "undergrad".into(),
            crate::models::StudentType::Grad => "grad".into(),
        }),
        phone: token.phone,
        access_token: None,
        refresh_token: None,
        cookies: cookies.iter().map(|c| (c.name.clone(), c.value.clone())).collect(),
    })
}

pub async fn login_and_fetch_info_via_browser(
    username: Option<&str>,
    password: Option<&str>,
    info_url: Option<&str>,
    date_from: Option<&str>,
    date_to: Option<&str>,
    interactive: bool,
) -> Result<AuthenticatedFetchResult> {
    let (mut browser, handler_task, should_close) = open_browser().await?;

    let page = browser
        .new_page(JW_CAS_URL)
        .await
        .context("failed to open JW CAS page in browser")?;

    ensure_logged_in(&page, username, password, interactive).await?;

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

    // Save cookies for cron reuse (no browser needed next time)
    if let Err(e) = save_cookies_to_file(&cookies, "session-cookies.json") {
        eprintln!("warning: failed to save session cookies: {e}");
    } else {
        eprintln!("session cookies saved to session-cookies.json");
    }

    let target_url = info_url.unwrap_or(INFO_DEFAULT_URL);
    let target_page = browser
        .new_page(target_url)
        .await
        .with_context(|| format!("failed to open target page {target_url} in browser"))?;
    sleep(Duration::from_secs(3)).await;

    let first_url_str = evaluate_string_with_retry(&target_page, "() => window.location.href")
        .await
        .context("failed to read final target page URL")?;
    let first_html = page_content_with_retry(&target_page)
        .await
        .with_context(|| format!("failed to read target page HTML from {target_url}"))?;

    let first_url = Url::parse(&first_url_str).with_context(|| {
        format!("browser returned invalid final target page URL: {first_url_str}")
    })?;
    let fetched_page = extract_page_snapshot(&first_url, &first_html);

    // --- date-range notice extraction with pagination ---
    let date_notices = if date_from.is_some() || date_to.is_some() {
        let lower = date_from.unwrap_or("0000-01-01");
        let upper = date_to.unwrap_or("9999-12-31");
        let mut all_notices = extract_notice_items(&first_html, &first_url);
        let mut pages_fetched: usize = 1;
        let mut current_url = first_url.clone();

        // Paginate while oldest notice on the current page >= lower bound.
        // (Notices are newest-first, so .last() is the oldest on the page.)
        loop {
            let oldest = all_notices.last().map(|n| n.date.as_str());
            if oldest.is_none_or(|d| d < lower) {
                break;
            }

            let next_url = if pages_fetched == 1 {
                find_next_page_url(&first_html, &current_url)
            } else {
                let cur_html = page_content_with_retry(&target_page).await?;
                find_next_page_url(&cur_html, &current_url)
            };

            let Some(next_url) = next_url else {
                break;
            };

            eprintln!(
                "paginating to page {}: {}",
                pages_fetched + 1,
                next_url.as_str()
            );

            // Navigate via JS assignment (page context destroyed on navigation,
            // so fire-and-forget — do not expect a return value).
            let _ = target_page
                .evaluate_function(&format!(
                    "() => {{ window.location.href = {:?}; }}",
                    next_url.as_str(),
                ))
                .await;
            sleep(Duration::from_secs(3)).await;

            let next_html = page_content_with_retry(&target_page)
                .await
                .with_context(|| {
                    format!("failed to read page HTML from {}", next_url.as_str())
                })?;

            let page_notices = extract_notice_items(&next_html, &next_url);
            if page_notices.is_empty() {
                break;
            }
            all_notices.extend(page_notices);
            pages_fetched += 1;
            current_url = next_url;
        }

        // Filter to [lower, upper] inclusive
        all_notices.retain(|n| n.date.as_str() >= lower && n.date.as_str() <= upper);
        eprintln!(
            "date-range [{}, {}]: {} notices from {} page(s)",
            lower,
            upper,
            all_notices.len(),
            pages_fetched
        );
        Some(NoticeList {
            notices: all_notices,
            pages_fetched,
        })
    } else {
        None
    };

    if should_close {
        browser.close().await.ok();
    }
    handler_task.abort();

    Ok(AuthenticatedFetchResult {
        token,
        fetched_page,
        today_notices: date_notices,
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
    interactive: bool,
) -> Result<()> {
    if is_jw_page(page).await?
        || wait_for_existing_session(page, Duration::from_secs(EXISTING_BROWSER_WAIT_SECS)).await?
    {
        return Ok(());
    }

    // Browser may have cached cookies from a previous session,
    // landing directly on the multifactor page — skip credential form.
    if is_multifactor_page(page).await? {
        return wait_for_login_completion(page, interactive).await;
    }

    let username = username.context(
        "no authenticated Chrome session found; pass --username or set HITSZ_USERNAME to fall back to browser login",
    )?;
    let password = password.context(
        "no authenticated Chrome session found; pass --password or set HITSZ_PASSWORD to fall back to browser login",
    )?;

    if wait_for_selector(page, "#password", Duration::from_secs(3)).await.is_err() {
        // Page may default to QR-code tab; click "账号登录" or "Account login"
        if click_control_by_text(page, "账号登录").await.is_err() {
            click_control_by_text(page, "Account login").await?;
        }
        wait_for_selector(page, "#password", Duration::from_secs(20)).await?;
    }

    fill_credentials(page, username, password).await?;
    // Click login button directly by ID (more reliable than text search)
    click_login_button(page).await?;
    wait_for_login_completion(page, interactive).await
}

async fn is_multifactor_page(page: &chromiumoxide::Page) -> Result<bool> {
    let html = match page.content().await {
        Ok(html) => html,
        Err(_) => return Ok(false),
    };
    Ok(is_multifactor_html(&html))
}

fn is_multifactor_html(html: &str) -> bool {
    // The IDS login page itself contains "动态码" as a tab label,
    // so we require the reAuthCheck marker to avoid false positives.
    html.contains("reAuthCheck")
        || html.contains("reAuthLoginView")
        || (html.contains("两步验证") && !html.contains("请输入学号"))
        || (html.contains("二次验证") && !html.contains("请输入学号"))
        || (html.contains("双因素") && !html.contains("请输入学号"))
        || (html.contains("multifactor") && !html.contains("passwordText"))
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

async fn wait_for_login_completion(page: &chromiumoxide::Page, interactive: bool) -> Result<()> {
    let started = std::time::Instant::now();
    let mut captcha_announced = false;
    let mut two_factor_announced = false;
    let mut two_factor_triggered = false;
    let mut two_factor_submitted = false;
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
        if is_multifactor_html(&current_html) {
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
                two_factor_announced = true;
            }
            if !two_factor_triggered {
                if request_two_factor_code(page).await.unwrap_or(false) {
                    eprintln!("requested verification code from multifactor page");
                }
                two_factor_triggered = true;
            }
            if !two_factor_submitted {
                if interactive {
                    match prompt_and_fill_two_factor(page).await {
                        Ok(true) => {
                            two_factor_submitted = true;
                            eprintln!("verification code submitted, waiting for redirect...");
                        }
                        Ok(false) => {
                            sleep(Duration::from_secs(2)).await;
                        }
                        Err(e) => {
                            eprintln!("two-factor input: {e}");
                        }
                    }
                } else {
                    eprintln!("multifactor detected; waiting (non-interactive mode)");
                    two_factor_submitted = true;
                }
            }
        }
        if current_html.contains("验证码") && current_html.to_lowercase().contains("captcha") {
            if !captcha_announced {
                eprintln!(
                    "captcha detected; complete it in the visible browser window or retry without --accept-invalid-certs"
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

/// Reads a verification code from terminal stdin and fills it into the 2FA page.
/// Returns `Ok(true)` when successfully submitted, `Ok(false)` when input was empty.
async fn prompt_and_fill_two_factor(page: &chromiumoxide::Page) -> Result<bool> {
    print!("Enter verification code: ");
    io::stdout().flush().context("failed to flush stdout")?;
    let mut code = String::new();
    io::stdin()
        .read_line(&mut code)
        .context("failed to read verification code from stdin")?;
    let code = code.trim().to_owned();
    if code.is_empty() {
        return Ok(false);
    }

    let code_escaped = serde_json::to_string(&code)?;
    let result = page
        .evaluate_function(&format!(
            r#"() => {{
                const code = {};
                // Try selectors for the 2FA code input
                const inputSelectors = [
                    '#code', '#verificationCode', '#totp', '#authcode', '#captcha',
                    'input[name="code"]', 'input[name="verificationCode"]',
                    'input[name="totp"]', 'input[name="authcode"]', 'input[name="captcha"]',
                    'input[placeholder*="动态码"]', 'input[placeholder*="验证码"]',
                ];
                let input = null;
                for (const sel of inputSelectors) {{
                    try {{ input = document.querySelector(sel); }} catch(_) {{}}
                    if (input) break;
                }}
                // Fallback: any visible text input not named username/password
                if (!input) {{
                    input = Array.from(document.querySelectorAll('input[type="text"]')).find(el => {{
                        const name = (el.name || '').toLowerCase();
                        const id = (el.id || '').toLowerCase();
                        return name !== 'username' && id !== 'username'
                            && name !== 'password' && id !== 'password';
                    }});
                }}
                if (!input) return {{ ok: false, reason: 'no code input found' }};

                input.focus();
                input.value = code;
                input.dispatchEvent(new Event('input', {{ bubbles: true }}));
                input.dispatchEvent(new Event('change', {{ bubbles: true }}));

                // Try to click submit
                const btnSelectors = [
                    '#submit', '#verify', '#confirm', '#login_submit',
                    'button[type="submit"]', 'input[type="submit"]',
                ];
                let btn = null;
                for (const sel of btnSelectors) {{
                    try {{ btn = document.querySelector(sel); }} catch(_) {{}}
                    if (btn) break;
                }}
                // Fallback: find button/link with submit-like text
                if (!btn) {{
                    const candidates = Array.from(document.querySelectorAll('a, button, input[type="submit"], input[type="button"]'));
                    btn = candidates.find(el => {{
                        const text = (el.innerText || el.value || '').trim();
                        return text === '确认' || text === '提交' || text === '验证'
                            || text === 'Verify' || text === 'Submit' || text === 'Confirm';
                    }});
                }}
                if (btn) {{
                    btn.click();
                    return {{ ok: true, submitted: true }};
                }}
                return {{ ok: true, submitted: false }};
            }}"#,
            code_escaped,
        ))
        .await
        .context("failed to fill verification code in browser")?;

    let parsed: Value = result
        .into_value()
        .context("verification code fill result was not valid JSON")?;
    let submitted = parsed
        .get("submitted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !submitted {
        eprintln!("filled verification code but could not auto-submit; click submit in browser window");
    }
    Ok(true)
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

async fn click_login_button(page: &chromiumoxide::Page) -> Result<()> {
    // The IDS login button is <a id="login_submit" class="login-btn">Login</a>
    page
        .evaluate_function(
            r#"() => {
                const btn = document.getElementById('login_submit')
                    || document.querySelector('a.login-btn')
                    || document.querySelector('button[type="submit"]');
                if (!btn) {
                    // Fallback: find by text
                    const candidates = Array.from(document.querySelectorAll('a,button'));
                    const textBtn = candidates.find(el =>
                        ['登录','Login','login'].includes((el.innerText || '').trim())
                    );
                    if (textBtn) { textBtn.click(); return 'text'; }
                    throw new Error('Could not find login button');
                }
                btn.click();
                return 'id';
            }"#,
        )
        .await
        .context("failed to click login button")?;
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
            // The IDS page has id="username" (text) and id="password" (name="passwordText", visible).
            // A hidden id="saltPassword" (name="password") gets populated by page JS on submit.
            // We fill the visible inputs and dispatch proper events so the page's
            // encryption listener picks up the password.
            const usernameInput = document.getElementById('username');
            const passwordInput = document.getElementById('password');
            if (!usernameInput || !passwordInput) {{
                throw new Error('Could not find #username or #password inputs');
            }}
            const applyValue = (node, value) => {{
                node.focus();
                node.value = value;
                // Use native setter to bypass React/framework overrides
                const nativeSetter = Object.getOwnPropertyDescriptor(
                    window.HTMLInputElement.prototype, 'value'
                ).set;
                nativeSetter.call(node, value);
                node.dispatchEvent(new Event('input', {{ bubbles: true }}));
                node.dispatchEvent(new Event('change', {{ bubbles: true }}));
                node.dispatchEvent(new Event('blur', {{ bubbles: true }}));
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
        .context("failed to fill username/password in browser form")?;
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
        "filled login form: username_len={}, password_len={}, username_matches={}",
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

// ── cookie persistence for cron reuse ───────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedCookie {
    name: String,
    value: String,
    domain: String,
    path: String,
}

fn save_cookies_to_file(cookies: &[Cookie], path: &str) -> Result<()> {
    let saved: Vec<SavedCookie> = cookies
        .iter()
        .map(|c| SavedCookie {
            name: c.name.clone(),
            value: c.value.clone(),
            domain: c.domain.clone(),
            path: c.path.clone(),
        })
        .collect();
    let json = serde_json::to_string_pretty(&saved)?;
    std::fs::write(path, json)
        .with_context(|| format!("failed to write cookie file {path}"))?;
    Ok(())
}


fn extract_js_redirect(html: &str) -> Option<String> {
    // Parse window.location.href='...' or window.location.href="..."
    let marker = "window.location.href=";
    let pos = html.find(marker)?;
    let rest = &html[pos + marker.len()..];
    let quote = rest.chars().next()?;
    if quote != '\'' && quote != '"' { return None; }
    let end = rest[1..].find(quote)?;
    Some(rest[1..1 + end].to_string())
}
pub async fn fetch_info_with_saved_cookies(
    cookie_file: &str,
    info_url: &str,
    date_from: Option<&str>,
    date_to: Option<&str>,
) -> Result<Option<AuthenticatedFetchResult>> {
    use reqwest_cookie_store::{CookieStore, CookieStoreMutex};
    use std::sync::Arc;

    let raw = std::fs::read_to_string(cookie_file)
        .with_context(|| format!("failed to read cookie file {cookie_file}"))?;
    let saved: Vec<SavedCookie> = serde_json::from_str(&raw)
        .context("failed to parse cookie file")?;

    // Build a proper cookie store so domain-scoped cookies (CASTGC on
    // ids.hit.edu.cn, JSESSIONID on jw.hitsz.edu.cn) are sent correctly
    // when reqwest follows the CAS redirect chain.
    let mut store = CookieStore::default();
    for c in &saved {
        let domain = if c.domain.starts_with('.') {
            c.domain.clone()
        } else {
            c.domain.clone()
        };
        let url = format!("https://{domain}/");
        if let Ok(url) = Url::parse(&url) {
            let cookie_str = format!("{}={}; Path={}", c.name, c.value, c.path);
            let _ = store.parse(&cookie_str, &url);
        }
    }
    let store_arc = Arc::new(CookieStoreMutex::new(store));

    let client = reqwest::Client::builder()
        .cookie_provider(store_arc.clone())
        .danger_accept_invalid_certs(true)
        .build()
        .context("failed to build HTTP client")?;

    let resp = client
        .get(info_url)
        .send()
        .await
        .context("failed to fetch info page with saved cookies")?;

    let mut final_url = resp.url().clone();
    let mut html = resp.text().await?;

    // The info portal uses JS redirects (window.location.href=...) for CAS,
    // which reqwest can't follow. Parse and follow manually.
    if html.contains("window.location.href") && !html.contains("Newslist") {
        if let Some(js_url) = extract_js_redirect(&html) {
            eprintln!("following JS redirect to {}", js_url);
            let resp2 = client.get(&js_url).send().await
                .context("failed to follow JS redirect")?;
            final_url = resp2.url().clone();
            html = resp2.text().await?;
        }
    }

    // May need one more hop through CAS
    if html.contains("window.location.href") && !html.contains("Newslist") {
        if let Some(js_url) = extract_js_redirect(&html) {
            eprintln!("following JS redirect to {}", js_url);
            let resp3 = client.get(&js_url).send().await?;
            final_url = resp3.url().clone();
            html = resp3.text().await?;
        }
    }

    // Check if we got redirected to CAS login (cookies expired)
    if final_url.host_str() != Some("info.hitsz.edu.cn")
        || html.contains("authserver/login")
        || (html.contains("caslogin") && !html.contains("Newslist"))
    {
        eprintln!("saved cookies expired (redirected to CAS login)");
        return Ok(None);
    }

    let final_url = Url::parse(&final_url.to_string())?;
    let fetched_page = extract_page_snapshot(&final_url, &html);

    // Date-range notice extraction with pagination
    let date_notices = if date_from.is_some() || date_to.is_some() {
        let lower = date_from.unwrap_or("0000-01-01");
        let upper = date_to.unwrap_or("9999-12-31");
        let mut all_notices = extract_notice_items(&html, &final_url);
        let mut pages_fetched: usize = 1;
        let mut current_url = final_url.clone();

        loop {
            let oldest = all_notices.last().map(|n| n.date.as_str());
            if oldest.is_none_or(|d| d < lower) {
                break;
            }
            let next_url = find_next_page_url(&html, &current_url);
            let Some(next_url) = next_url else { break };

            eprintln!("paginating to page {}: {}", pages_fetched + 1, next_url.as_str());

            let next_resp = client.get(next_url.as_str()).send().await?;
            let next_html = next_resp.text().await?;

            let page_notices = extract_notice_items(&next_html, &next_url);
            if page_notices.is_empty() { break }
            all_notices.extend(page_notices);
            pages_fetched += 1;
            current_url = next_url;
        }

        all_notices.retain(|n| n.date.as_str() >= lower && n.date.as_str() <= upper);
        eprintln!("date-range [{}, {}]: {} notices from {} page(s)", lower, upper, all_notices.len(), pages_fetched);
        Some(NoticeList { notices: all_notices, pages_fetched })
    } else {
        None
    };

    let token = EasToken {
        cookies: saved.iter().map(|c| (c.name.clone(), c.value.clone())).collect(),
        username: String::new(), password: String::new(),
        name: None, stutype: StudentType::Undergrad,
        picture: None, id: None, stu_id: None,
        school: None, major: None, grade: None,
        sfxsx: None, email: None, phone: None,
    };

    Ok(Some(AuthenticatedFetchResult {
        token, fetched_page, today_notices: date_notices,
    }))
}
