//! Pure HTTP CAS/IDS login (no browser). Walks the IDS login form, submits
//! credentials with AES-128-CBC encryption, follows CAS redirects to the JW
//! portal, fetches profile JSON, and retrieves the info portal notice page.

use crate::extract::extract_page_snapshot;
use crate::models::{AuthenticatedFetchResult, EasToken, StudentType};
use anyhow::{Context, Result, bail};
use reqwest::blocking::{Client, ClientBuilder};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONNECTION, USER_AGENT};
use reqwest_cookie_store::{CookieStore, CookieStoreMutex};
use scraper::{Html, Selector};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use url::Url;

const IDS_HOST: &str = "https://ids.hit.edu.cn";
const IDS_LOGIN_PATH: &str = "/authserver/login";
const JW_PROFILE_URL: &str = "http://jw.hitsz.edu.cn/UserManager/queryxsxx";
const INFO_DEFAULT_URL: &str = "https://info.hitsz.edu.cn/";
const JW_HOST: &str = "jw.hitsz.edu.cn";
const INFO_HOST: &str = "info.hitsz.edu.cn";

// ── types ───────────────────────────────────────────────────────────────────

struct ParsedForm {
    action: String,
    /// All hidden `<input name=… value=…>` fields captured verbatim.
    fields: Vec<(String, String)>,
    pwd_encrypt_salt: String,
}

pub struct AuthSession {
    client: Client,
    cookie_store: Arc<CookieStoreMutex>,
}

// ── public API ──────────────────────────────────────────────────────────────

impl AuthSession {
    pub fn new(accept_invalid_certs: bool) -> Result<Self> {
        let cookie_store = Arc::new(CookieStoreMutex::new(CookieStore::default()));
        let client = ClientBuilder::new()
            .default_headers(default_headers())
            .cookie_provider(cookie_store.clone())
            .danger_accept_invalid_certs(accept_invalid_certs)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self { client, cookie_store })
    }

    pub fn login_and_fetch_info(
        &self,
        username: &str,
        password: &str,
        info_url: Option<&str>,
    ) -> Result<AuthenticatedFetchResult> {
        self.cas_login(username, password)?;
        let token = self.fetch_profile(username, password)?;
        let target = info_url.unwrap_or(INFO_DEFAULT_URL);
        let page = self.fetch_info_page(target)?;
        Ok(AuthenticatedFetchResult { token, fetched_page: page, today_notices: None })
    }
}

// ── CAS login ──────────────────────────────────────────────────────────────

impl AuthSession {
    fn cas_login(&self, username: &str, password: &str) -> Result<()> {
        // Use a redirect-following client so CAS ticket validation
        // and JW session cookies are handled automatically.
        let rd = ClientBuilder::new()
            .default_headers(default_headers())
            .cookie_provider(self.cookie_store.clone())
            .danger_accept_invalid_certs(true)
            .build()
            .context("failed to build redirect client")?;

        let service = "http://jw.hitsz.edu.cn/casLogin";
        let page_url = format!(
            "{IDS_HOST}{IDS_LOGIN_PATH}?type=userNameLogin&service={}",
            url_encode(service)
        );

        // Step 1 — GET login form
        let html = rd.get(&page_url).send()
            .context("failed to load IDS login page")?
            .error_for_status()
            .context("IDS login page returned error")?
            .text()
            .context("failed to read login page HTML")?;

        let form = parse_form(&html)?;
        let action = if form.action.starts_with('/') {
            format!("{IDS_HOST}{}", form.action)
        } else if form.action.is_empty() {
            page_url
        } else {
            form.action
        };

        // Step 2 — POST credentials (client follows CAS redirects automatically)
        let mut params: Vec<(&str, String)> = form.fields.iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        params.push(("username", username.to_string()));
        if !form.pwd_encrypt_salt.is_empty() {
            params.push(("password", encrypt_password(password, &form.pwd_encrypt_salt)?));
        }
        params.push(("rememberMe", "true".to_string()));

        let resp = rd.post(&action)
            .form(&params.iter().map(|(k, v)| (*k, v.as_str())).collect::<Vec<_>>())
            .send()
            .with_context(|| format!("failed to POST login to {action}"))?;

        // Check final URL: if on JW, login succeeded
        let final_url = resp.url().to_string();
        let status = resp.status();
        eprintln!("[CAS] final_url after POST: {final_url}");
        if final_url.contains(JW_HOST) {
            return Ok(());
        }

        let body = resp.text().unwrap_or_default();
        if body.contains("用户名或密码错误") || body.contains("账号或密码错误")
            || body.contains("username or password is incorrect")
        {
            bail!("CAS login: incorrect username or password");
        }
        bail!(
            "CAS login: ended at {final_url} (status {status}), expected JW redirect"
        );
    }

}

// ── profile + info ─────────────────────────────────────────────────────────

impl AuthSession {
    fn fetch_profile(&self, username: &str, password: &str) -> Result<EasToken> {
        let rd = ClientBuilder::new()
            .default_headers(default_headers())
            .cookie_provider(self.cookie_store.clone())
            .danger_accept_invalid_certs(true)
            .build()?;

        // Debug: dump cookies before profile fetch
        {
            let store = self.cookie_store.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            eprintln!("[profile debug] cookies:");
            for c in store.iter_any() {
                eprintln!("  domain={:?} name={} path={:?}",
                    c.domain(), c.name(), c.path());
            }
        }

        let json = rd.post(JW_PROFILE_URL)
            .header("X-Requested-With", "XMLHttpRequest")
            .send()
            .context("profile fetch failed")?
            .error_for_status()
            .context("profile request returned error status")?
            .text()?;

        let cookies = extract_domain_cookies(&self.cookie_store, JW_HOST)?;
        if json.contains("session已失效") { bail!("profile: session expired"); }
        if json.trim_start().starts_with('<') { bail!("profile: got HTML instead of JSON"); }
        build_eas_token(username, password, cookies, &json)
    }

    fn fetch_info_page(&self, url: &str) -> Result<crate::models::PageSnapshot> {
        let rd = ClientBuilder::new()
            .default_headers(default_headers())
            .cookie_provider(self.cookie_store.clone())
            .danger_accept_invalid_certs(true)
            .build()?;
        let resp = rd.get(url).send()
            .with_context(|| format!("info page fetch {url}"))?;
        let final_url = resp.url().clone();
        let html = resp.error_for_status()
            .with_context(|| format!("info page {url} error"))?
            .text()?;
        ensure_authenticated(&final_url, &html)?;
        Ok(extract_page_snapshot(&final_url, &html))
    }
}

// ── form parsing ────────────────────────────────────────────────────────────

fn parse_form(html: &str) -> Result<ParsedForm> {
    let doc = Html::parse_document(html);
    let input_sel = Selector::parse("input").expect("valid");

    let mut fields = Vec::new();
    let mut pwd_salt = String::new();
    for el in doc.select(&input_sel) {
        let value = el.value().attr("value").unwrap_or("").to_string();
        // pwdEncryptSalt may have no `name`, capture by id first
        if el.value().id() == Some("pwdEncryptSalt") && !value.is_empty() {
            pwd_salt = value;
            continue;
        }
        let name = el.value().attr("name").unwrap_or("").to_string();
        if name.is_empty() { continue; }
        // Skip fields we'll supply ourselves
        if name == "passwordText" || name == "password" || name == "username" || name == "rememberMe" { continue; }
        // Keep LAST occurrence of each name (userNameLogin section overwrites earlier tabs)
        fields.retain(|(n, _)| n != &name);
        fields.push((name, value));
    }

    let form_sel = Selector::parse("form").expect("valid");
    let action = doc.select(&form_sel).next()
        .and_then(|f| f.value().attr("action"))
        .unwrap_or("")
        .to_string();

    Ok(ParsedForm { action, fields, pwd_encrypt_salt: pwd_salt })
}

// ── encryption ──────────────────────────────────────────────────────────────
use aes::cipher::{BlockModeEncrypt, Iv, Key, KeyIvInit, block_padding::Pkcs7};
use aes::Aes128;
use base64::Engine;
use cbc::Encryptor;
use rand::RngExt;
type Aes128Cbc = Encryptor<Aes128>;
fn encrypt_password(password: &str, salt: &str) -> Result<String> {
    let key = Key::<Aes128Cbc>::try_from(&salt.as_bytes()[..16])
        .map_err(|_| anyhow::anyhow!("key too short"))?;
    let charset: &[u8] = b"ABCDEFGHJKMNPQRSTWXYZabcdefhijkmnprstwxyz2345678";
    let mut rng = rand::rng();

    let iv_str: String = (0..16)
        .map(|_| charset[rng.random_range(0..charset.len())] as char)
        .collect();
    let iv = Iv::<Aes128Cbc>::try_from(iv_str.as_bytes())
        .map_err(|_| anyhow::anyhow!("iv too short"))?;

    let padding: String = (0..64)
        .map(|_| charset[rng.random_range(0..charset.len())] as char)
        .collect();
    let plaintext = format!("{}{}", padding, password);
    let plain_bytes = plaintext.as_bytes();

    let buf_len = plain_bytes.len() + 16;
    let mut buf = vec![0u8; buf_len];
    buf[..plain_bytes.len()].copy_from_slice(plain_bytes);

    let ct = Aes128Cbc::new(&key, &iv)
        .encrypt_padded::<Pkcs7>(&mut buf, plain_bytes.len())
        .map_err(|e| anyhow::anyhow!("AES encrypt: {e:?}"))?;

    Ok(base64::engine::general_purpose::STANDARD.encode(ct))
}
// ── helpers ─────────────────────────────────────────────────────────────────

fn default_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(ACCEPT, HeaderValue::from_static("*/*"));
    h.insert(CONNECTION, HeaderValue::from_static("keep-alive"));
    h.insert(USER_AGENT, HeaderValue::from_static(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
    ));
    h
}

fn extract_domain_cookies(store: &Arc<CookieStoreMutex>, host: &str) -> Result<HashMap<String, String>> {
    let store = store.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut map = HashMap::new();
    for c in store.iter_any() {
        // Accept host-only cookies (domain=None), exact match, and parent domain
        let matches = c.domain().is_none()
            || c.domain().as_deref() == Some(host)
            || c.domain().map_or(false, |d| host.ends_with(d) || d.ends_with(host));
        if matches {
            map.insert(c.name().to_string(), c.value().to_string());
        }
    }
    Ok(map)
}

fn build_eas_token(
    username: &str, password: &str,
    cookies: HashMap<String, String>, profile_json: &str,
) -> Result<EasToken> {
    let p: Value = serde_json::from_str(profile_json)?;
    Ok(EasToken {
        cookies, username: username.into(), password: password.into(),
        name: str_val(&p, "XM"),
        stutype: match str_val(&p, "PYLX").as_deref() {
            Some("1") => StudentType::Undergrad,
            Some("2") => StudentType::Grad,
            _ => StudentType::Undergrad,
        },
        picture: str_val(&p, "ZPBSLJ"),
        id: str_val(&p, "ID"),
        stu_id: str_val(&p, "XH"),
        school: str_val(&p, "YXMC"),
        major: str_val(&p, "ZYMC"),
        grade: str_val(&p, "NJMC"),
        sfxsx: str_val(&p, "sfxsx"),
        email: str_val(&p, "DZYX"),
        phone: str_val(&p, "LXDH"),
    })
}

fn str_val(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn ensure_authenticated(final_url: &Url, html: &str) -> Result<()> {
    if final_url.host_str() != Some(INFO_HOST) {
        bail!("unexpected info page host: {:?}", final_url.host_str());
    }
    if html.contains("authserver/login") {
        bail!("info page redirected to CAS login (session expired)");
    }
    Ok(())
}

fn url_encode(s: &str) -> String {
    s.replace(':', "%3A").replace('/', "%2F").replace('?', "%3F")
     .replace('=', "%3D").replace('&', "%26")
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_login_form_fields() {
        let html = r#"<html><body><form action="/authserver/login">
            <input name="lt" value="LT-abc"/>
            <input name="execution" value="e1s1"/>
            <input name="_eventId" value="submit"/>
            <input id="pwdEncryptSalt" value="0123456789abcdef"/>
        </form></body></html>"#;
        let form = parse_form(html).expect("parse");
        assert_eq!(form.pwd_encrypt_salt, "0123456789abcdef");
        assert!(form.fields.iter().any(|(n, v)| n == "lt" && v == "LT-abc"));
    }

    #[test]
    fn encrypts_password() {
        let salt = "0123456789abcdef"; // 16 bytes
        let result = encrypt_password("test", salt).expect("encrypt");
        assert!(!result.is_empty());
        assert!(result.len() > 50); // should be base64 of ~80+ bytes
    }

    #[test]
    fn builds_token_from_profile() {
        let json = r#"{"PYLX":"1","YXMC":"计算机学院","XH":"20220001","XM":"张三","sfxsx":"0"}"#;
        let tok = build_eas_token("alice", "s", HashMap::new(), json).expect("build");
        assert!(matches!(tok.stutype, StudentType::Undergrad));
        assert_eq!(tok.name.as_deref(), Some("张三"));
    }
}
