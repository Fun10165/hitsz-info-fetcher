//! HIT unified identity authentication.
//!
//! Two login paths:
//!
//! | Method | 2FA | Browser | Network | Target |
//! |--------|-----|---------|---------|--------|
//! | [`login_via_browser`] | handled | required | IDS CAS | JW + info portal |
//! | [`login_via_mjw`] | not needed | none | mjw API | JW only |
//!
//! # Example
//!
//! ```ignore
//! use hitsz_info_fetcher::hit_auth::login_via_mjw;
//! let token = login_via_mjw("学号", "密码").expect("login failed");
//! println!("Hello, {:?}", token.name);
//! ```
//!
//! # Browser example
//!
//! ```ignore
//! use hitsz_info_fetcher::hit_auth::login_via_browser;
//! #[tokio::main]
//! async fn main() {
//!     let session = login_via_browser("学号", "密码").await.unwrap();
//!     println!("{:?}", session.token.name);
//! }
//! ```

use anyhow::{Context, Result, bail};
use reqwest::blocking::{Client, ClientBuilder};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── public types ────────────────────────────────────────────────────────────

/// Authentication token from either mjw or browser login.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HitToken {
    pub username: String,
    pub name: Option<String>,
    pub student_id: Option<String>,
    pub school: Option<String>,
    pub stutype: Option<String>,
    pub phone: Option<String>,

    /// Present only from mjw login
    pub access_token: Option<String>,
    /// Present only from mjw login
    pub refresh_token: Option<String>,
    /// Session cookies for authenticated HTTP requests
    pub cookies: HashMap<String, String>,
}

/// Authenticated session — holds token + HTTP client with cookies.
/// Use `session.client` to make requests to JW / info portal.
#[derive(Debug, Clone)]
pub struct HitSession {
    pub token: HitToken,
    pub client: Client,
}

// ── MJW login (no browser) ──────────────────────────────────────────────────

const MJW_BASE: &str = "https://mjw.hitsz.edu.cn/incoSpringBoot";
const BASIC_AUTH: &str = "Basic aW5jb246MTIzNDU=";
const MJW_USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 15; V2183A Build/AP3A.240905.015.A2; wv) \
    AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/144.0.7559.132 Mobile Safari/537.36 \
    uni-app Html5Plus/1.0 (Immersed/38.0)";

/// Login via mjw API — no browser, no 2FA.
/// Only works for `mjw.hitsz.edu.cn/incoSpringBoot/` services (教务).
pub fn login_via_mjw(username: &str, password: &str) -> Result<HitSession> {
    let client = ClientBuilder::new()
        .cookie_store(true)
        .danger_accept_invalid_certs(true)
        .user_agent(MJW_USER_AGENT)
        .build()
        .context("failed to build HTTP client")?;

    // Step 1 — bootstrap
    client
        .post(format!("{MJW_BASE}/component/queryApplicationSetting/rsa"))
        .header("Authorization", BASIC_AUTH)
        .header("rolecode", "01")
        .header("_lang", "cn")
        .body("")
        .send()
        .context("mjw bootstrap failed")?;

    // Step 2 — RSA key (best-effort)
    let _ = client
        .post(format!("{MJW_BASE}/c_raskey"))
        .header("Authorization", BASIC_AUTH)
        .header("rolecode", "06")
        .header("_lang", "cn")
        .body("")
        .send();

    let token = try_mjw_ldap(&client, username, password)?;

    // Build a client for authenticated requests
    let session_client = ClientBuilder::new()
        .danger_accept_invalid_certs(true)
        .default_headers({
            let mut h = reqwest::header::HeaderMap::new();
            h.insert(
                reqwest::header::AUTHORIZATION,
                reqwest::header::HeaderValue::from_str(
                    &format!("bearer {}", token.access_token.as_deref().unwrap_or(""))
                )?,
            );
            h.insert("rolecode", reqwest::header::HeaderValue::from_static("06"));
            h.insert("_lang", reqwest::header::HeaderValue::from_static("cn"));
            h
        })
        .build()?;

    Ok(HitSession { token, client: session_client })
}

fn try_mjw_ldap(client: &Client, username: &str, password: &str) -> Result<HitToken> {
    for &rc in &["06", "01"] {
        let resp = client
            .post(format!("{MJW_BASE}/authentication/ldap"))
            .header("Authorization", BASIC_AUTH)
            .header("rolecode", rc)
            .header("_lang", "cn")
            .form(&[("username", username), ("password", password)])
            .send()
            .context("mjw LDAP request failed")?;

        let body = resp.text()?;
        if let Ok(ldap) = serde_json::from_str::<MjwLdapResponse>(&body) {
            if let Some(tok) = ldap.access_token.as_deref().filter(|t| !t.is_empty()) {
                let data = ldap.data.unwrap_or_default();
                let info = ldap.info.unwrap_or_default();
                return Ok(HitToken {
                    username: username.into(),
                    name: data.name.or(info.name),
                    student_id: info.stu_id,
                    school: data.school,
                    stutype: data.stutype,
                    phone: data.phone,
                    access_token: Some(tok.into()),
                    refresh_token: ldap.refresh_token,
                    cookies: HashMap::new(),
                });
            }
            if body.contains("用户名或密码错误") {
                bail!("mjw login: incorrect username or password");
            }
        }
    }
    bail!("mjw LDAP login failed for all rolecodes");
}

#[derive(Deserialize, Default)]
struct MjwLdapResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    data: Option<MjwLdapData>,
    info: Option<MjwLdapInfo>,
}

#[derive(Deserialize, Default)]
struct MjwLdapData {
    #[serde(rename = "yhxm")] name: Option<String>,
    #[serde(rename = "bmmc")] school: Option<String>,
    #[serde(rename = "lxdh")] phone: Option<String>,
    #[serde(rename = "pylx")] stutype: Option<String>,
}

#[derive(Deserialize, Default)]
struct MjwLdapInfo {
    #[serde(rename = "yhdm")] stu_id: Option<String>,
    #[serde(rename = "xm")]    name: Option<String>,
}

// ── Browser login ───────────────────────────────────────────────────────────

/// Login via Chrome browser — handles CAS + 2FA.
/// Returns a session with cookies for info portal access.
pub async fn login_via_browser(username: &str, password: &str) -> Result<HitSession> {
    use crate::browser_auth;
    let result = browser_auth::browser_login(username, password).await?;
    let cookies = result.cookies.clone();
    Ok(HitSession {
        token: result,
        client: build_browser_session_client(&cookies)?,
    })
}

fn build_browser_session_client(cookies: &HashMap<String, String>) -> Result<Client> {
    use reqwest_cookie_store::{CookieStore, CookieStoreMutex};
    use std::sync::Arc;
    use url::Url;

    let mut store = CookieStore::default();
    for (name, value) in cookies {
        // Try to insert for common HIT domains
        for domain in &["hitsz.edu.cn", "hit.edu.cn", "edu.cn"] {
            let url = format!("https://{domain}/");
            if let Ok(u) = Url::parse(&url) {
                let cookie_str = format!("{name}={value}; Path=/");
                let _ = store.parse(&cookie_str, &u);
            }
        }
    }

    let client = ClientBuilder::new()
        .cookie_provider(Arc::new(CookieStoreMutex::new(store)))
        .danger_accept_invalid_certs(true)
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
        .build()?;
    Ok(client)
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mjw_ldap_success_response() {
        let json = r#"{
            "access_token": "abc123def456", "refresh_token": "ref789",
            "data": { "pylx": "1", "yhxm": "王五", "bmmc": "信息学部" },
            "info": { "yhdm": "2023311001", "xm": "王五", "roles": ["01"] }
        }"#;
        let resp: MjwLdapResponse = serde_json::from_str(json).expect("parse");
        assert_eq!(resp.access_token.as_deref(), Some("abc123def456"));
    }

    #[test]
    fn parses_mjw_ldap_failure() {
        let resp: MjwLdapResponse = serde_json::from_str(r#"{"code":500,"msg":"error"}"#).expect("parse");
        assert!(resp.access_token.is_none());
    }

    #[test]
    fn token_equality() {
        let t1 = HitToken {
            username: "test".into(), name: Some("张三".into()),
            student_id: Some("20240001".into()), school: None,
            stutype: Some("undergrad".into()), phone: None,
            access_token: Some("tok".into()), refresh_token: None,
            cookies: HashMap::new(),
        };
        let mut t2 = t1.clone();
        t2.name = Some("李四".into());
        assert_ne!(t1, t2);
    }

    #[test]
    fn smoke_mjw_bootstrap_reachable() {
        let client = ClientBuilder::new().danger_accept_invalid_certs(true).cookie_store(true)
            .user_agent("Mozilla/5.0").build().expect("build");
        let resp = client.post(format!("{MJW_BASE}/component/queryApplicationSetting/rsa"))
            .header("Authorization", BASIC_AUTH).header("rolecode", "01").header("_lang", "cn").body("")
            .send().expect("POST");
        assert!(resp.status().is_success());
    }

    #[test]
    fn smoke_mjw_ldap_rejects_bad_creds() {
        let client = ClientBuilder::new().danger_accept_invalid_certs(true).cookie_store(true)
            .user_agent("Mozilla/5.0").build().expect("build");
        client.post(format!("{MJW_BASE}/component/queryApplicationSetting/rsa"))
            .header("Authorization", BASIC_AUTH).header("rolecode", "01").header("_lang", "cn").body("")
            .send().expect("bootstrap");
        let resp = client.post(format!("{MJW_BASE}/authentication/ldap"))
            .header("Authorization", BASIC_AUTH).header("rolecode", "06").header("_lang", "cn")
            .form(&[("username", "fake_99999"), ("password", "wrong")]).send().expect("LDAP");
        assert!(resp.text().unwrap_or_default().contains("用户名或密码错误"));
    }

    #[test]
    fn integration_mjw_login() {
        let u = std::env::var("HITSZ_USERNAME").expect("HITSZ_USERNAME required");
        let p = std::env::var("HITSZ_PASSWORD").expect("HITSZ_PASSWORD required");
        let session = login_via_mjw(&u, &p).expect("login");
        assert!(session.token.name.is_some());
        assert!(!session.token.access_token.as_deref().unwrap_or("").is_empty());
    }
}
