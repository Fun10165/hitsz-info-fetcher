use crate::extract::extract_page_snapshot;
use crate::models::{AuthenticatedFetchResult, EasToken, StudentType};
use anyhow::{Context, Result, anyhow, bail};
use reqwest::blocking::{Client, ClientBuilder};
use reqwest::header::{ACCEPT, CONNECTION, HeaderMap, HeaderValue, USER_AGENT};
use reqwest_cookie_store::{CookieStore, CookieStoreMutex};
use scraper::{Html, Selector};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use url::Url;

const JW_HOST: &str = "http://jw.hitsz.edu.cn";
const JW_CAS_URL: &str = "http://jw.hitsz.edu.cn/cas";
const COMBINED_LOGIN_URL: &str = "https://ids.hit.edu.cn/authserver/combinedLogin.do?type=IDSUnion&appId=ff2dfca3a2a2448e9026a8c6e38fa52b&success=http%3A%2F%2Fjw.hitsz.edu.cn%2FcasLogin";
const IDS_CALLBACK_URL: &str = "https://ids.hit.edu.cn/authserver/callback";
const INFO_DEFAULT_URL: &str = "https://info.hitsz.edu.cn/";
const INFO_HOST: &str = "info.hitsz.edu.cn";

#[derive(Debug, Clone)]
struct AuthForm {
    action: String,
    client_id: String,
    scope: String,
    state: String,
}

pub struct AuthSession {
    client: Client,
    cookie_store: Arc<CookieStoreMutex>,
}

impl AuthSession {
    pub fn new(accept_invalid_certs: bool) -> Result<Self> {
        let cookie_store = Arc::new(CookieStoreMutex::new(CookieStore::default()));
        let client = ClientBuilder::new()
            .default_headers(default_headers())
            .cookie_provider(cookie_store.clone())
            .danger_accept_invalid_certs(accept_invalid_certs)
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            client,
            cookie_store,
        })
    }

    pub fn login_and_fetch_info(
        &self,
        username: &str,
        password: &str,
        info_url: Option<&str>,
    ) -> Result<AuthenticatedFetchResult> {
        let cookies_before_login = extract_domain_cookies(&self.cookie_store, JW_HOST)?;
        self.client
            .get(JW_CAS_URL)
            .send()
            .context("failed to initialize JW CAS session")?
            .error_for_status()
            .context("JW CAS session initialization returned non-success status")?;

        let login_page_html = self
            .client
            .get(COMBINED_LOGIN_URL)
            .send()
            .context("failed to load IDS combined login page")?
            .error_for_status()
            .context("IDS combined login page returned non-success status")?
            .text()
            .context("failed to read IDS combined login HTML")?;

        let auth_form = parse_auth_form(&login_page_html)?;
        let sso_url = resolve_sso_url(&auth_form.action)?;

        let login_response = self
            .client
            .post(&sso_url)
            .form(&[
                ("action", "authorize"),
                ("response_type", "code"),
                ("redirect_uri", IDS_CALLBACK_URL),
                ("client_id", auth_form.client_id.as_str()),
                ("scope", auth_form.scope.as_str()),
                ("state", auth_form.state.as_str()),
                ("username", username),
                ("password", password),
            ])
            .send()
            .with_context(|| format!("failed to submit SSO login request to {sso_url}"))?
            .error_for_status()
            .with_context(|| {
                format!("SSO login request to {sso_url} returned non-success status")
            })?;

        let final_url = login_response.url().clone();
        if final_url.path() == "/authentication/main" {
            bail!("authentication failed: final redirect stayed on /authentication/main");
        }

        let cookies = extract_domain_cookies(&self.cookie_store, JW_HOST)?;
        if cookies.is_empty() {
            bail!("authentication did not yield any jw.hitsz.edu.cn cookies");
        }
        if cookies == cookies_before_login {
            bail!("authentication did not change jw.hitsz.edu.cn cookies after SSO submission");
        }

        let profile_json = self
            .client
            .post(format!("{JW_HOST}/UserManager/queryxsxx"))
            .header("X-Requested-With", "XMLHttpRequest")
            .send()
            .context("failed to request authenticated profile from jw.hitsz.edu.cn")?
            .error_for_status()
            .context("profile request returned non-success status")?
            .text()
            .context("failed to read authenticated profile response")?;

        if profile_json.contains("session已失效") {
            bail!("authenticated profile request reported expired session");
        }
        if profile_json.trim_start().starts_with('<') {
            bail!("authenticated profile request returned HTML instead of JSON");
        }

        let token = build_eas_token(username, password, cookies, &profile_json)?;
        if token.stu_id.as_deref().unwrap_or_default().is_empty() {
            bail!("authenticated profile JSON did not contain student id field XH");
        }

        let target_url = info_url.unwrap_or(INFO_DEFAULT_URL);
        let target_response = self
            .client
            .get(target_url)
            .send()
            .with_context(|| format!("failed to fetch target page {target_url}"))?;
        let target_final_url = target_response.url().clone();
        let target_html = target_response
            .error_for_status()
            .with_context(|| format!("target page {target_url} returned non-success status"))?
            .text()
            .with_context(|| format!("failed to read target page HTML from {target_url}"))?;
        ensure_authenticated_target(&target_final_url, &target_html)?;

        let fetched_page = extract_page_snapshot(&target_final_url, &target_html);

        Ok(AuthenticatedFetchResult {
            token,
            fetched_page,
            today_notices: None,
        })
    }
}

fn default_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
    headers.insert(CONNECTION, HeaderValue::from_static("keep-alive"));
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/79.0.3945.88 Safari/537.36",
        ),
    );
    headers
}

fn parse_auth_form(html: &str) -> Result<AuthForm> {
    let document = Html::parse_document(html);
    let form_selector = Selector::parse("#authZForm").expect("valid selector");
    let field_selector = Selector::parse("input[name]").expect("valid selector");

    let form = document
        .select(&form_selector)
        .next()
        .ok_or_else(|| anyhow!("could not find #authZForm on IDS combined login page"))?;
    let action = form
        .value()
        .attr("action")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("#authZForm is missing action attribute"))?
        .to_owned();

    let mut client_id = None;
    let mut scope = None;
    let mut state = None;
    for input in form.select(&field_selector) {
        let Some(name) = input.value().attr("name") else {
            continue;
        };
        let value = input.value().attr("value").unwrap_or_default().to_owned();
        match name {
            "client_id" => client_id = Some(value),
            "scope" => scope = Some(value),
            "state" => state = Some(value),
            _ => {}
        }
    }

    Ok(AuthForm {
        action,
        client_id: client_id.ok_or_else(|| anyhow!("missing hidden field client_id"))?,
        scope: scope.ok_or_else(|| anyhow!("missing hidden field scope"))?,
        state: state.ok_or_else(|| anyhow!("missing hidden field state"))?,
    })
}

fn resolve_sso_url(action: &str) -> Result<String> {
    let base =
        Url::parse("https://sso.hitsz.edu.cn:7002").context("failed to parse SSO base URL")?;
    Ok(base
        .join(action)
        .with_context(|| format!("failed to resolve SSO action URL from {action}"))?
        .to_string())
}

fn extract_domain_cookies(
    cookie_store: &Arc<CookieStoreMutex>,
    url: &str,
) -> Result<HashMap<String, String>> {
    let parsed =
        Url::parse(url).with_context(|| format!("invalid cookie extraction URL: {url}"))?;
    let store = cookie_store
        .lock()
        .map_err(|_| anyhow!("cookie store mutex was poisoned"))?;
    let values = store
        .get_request_values(&parsed)
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
    Ok(values)
}

fn build_eas_token(
    username: &str,
    password: &str,
    cookies: HashMap<String, String>,
    profile_json: &str,
) -> Result<EasToken> {
    let profile: Value = serde_json::from_str(profile_json)
        .with_context(|| format!("failed to parse profile JSON: {profile_json}"))?;

    let student_type = match profile.get("PYLX").and_then(Value::as_str) {
        Some("1") => StudentType::Undergrad,
        _ => StudentType::Grad,
    };

    Ok(EasToken {
        cookies,
        username: username.to_owned(),
        password: password.to_owned(),
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

fn get_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn ensure_authenticated_target(final_url: &Url, html: &str) -> Result<()> {
    let host = final_url.host_str().unwrap_or_default();
    if host != INFO_HOST {
        bail!(
            "target fetch finished on unexpected host {host}; authentication may not have reached info.hitsz.edu.cn"
        );
    }

    let lowered = html.to_ascii_lowercase();
    if lowered.contains("id=\"authzform\"")
        || lowered.contains("/authserver/combinedlogin.do")
        || lowered.contains("统一身份认证")
    {
        bail!(
            "target fetch appears to have landed on a login page instead of authenticated content"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{build_eas_token, parse_auth_form};
    use crate::models::StudentType;
    use std::collections::HashMap;

    #[test]
    fn parses_hidden_fields_from_auth_form() {
        let html = r#"
            <html>
              <body>
                <form id="authZForm" action="/oauth2/authorize">
                  <input name="client_id" value="client-1" />
                  <input name="scope" value="openid profile" />
                  <input name="state" value="state-1" />
                </form>
              </body>
            </html>
        "#;

        let form = parse_auth_form(html).expect("form should parse");
        assert_eq!(form.action, "/oauth2/authorize");
        assert_eq!(form.client_id, "client-1");
        assert_eq!(form.scope, "openid profile");
        assert_eq!(form.state, "state-1");
    }

    #[test]
    fn builds_token_from_profile_payload() {
        let profile = r#"{
            "PYLX": "1",
            "YXMC": "计算机学院",
            "ZYMC": "计算机科学与技术",
            "ZPBSLJ": "/avatar.jpg",
            "LXDH": "123456",
            "ID": "id-1",
            "DZYX": "test@example.com",
            "NJMC": "2022",
            "XH": "20220001",
            "XM": "张三",
            "sfxsx": "0"
        }"#;

        let token = build_eas_token("alice", "secret", HashMap::new(), profile)
            .expect("profile should parse");
        assert!(matches!(token.stutype, StudentType::Undergrad));
        assert_eq!(token.school.as_deref(), Some("计算机学院"));
        assert_eq!(token.stu_id.as_deref(), Some("20220001"));
        assert_eq!(token.name.as_deref(), Some("张三"));
    }
}
