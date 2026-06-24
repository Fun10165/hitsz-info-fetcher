//! Pure HTTP login to mjw.hitsz.edu.cn (no browser needed).
//! Reverse-engineered from the school's outsourced Android app (HITA_iOS).

use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;

const MJW_BASE: &str = "https://mjw.hitsz.edu.cn/incoSpringBoot";
const BASIC_AUTH: &str = "Basic aW5jb246MTIzNDU=";
const USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 15; V2183A Build/AP3A.240905.015.A2; wv) \
    AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/144.0.7559.132 Mobile Safari/537.36 \
    uni-app Html5Plus/1.0 (Immersed/38.0)";

// ── token types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MjwToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub name: Option<String>,
    pub school: Option<String>,
    pub stu_id: Option<String>,
    pub stutype: Option<String>,
    pub phone: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MjwSession {
    pub client: Client,
    pub token: MjwToken,
}

// ── deserialization helpers ─────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct LdapResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    data: Option<LdapData>,
    info: Option<LdapInfo>,
}

#[derive(Deserialize, Default)]
struct LdapData {
    #[serde(rename = "yhxm")]
    name: Option<String>,
    #[serde(rename = "bmmc")]
    school: Option<String>,
    #[serde(rename = "lxdh")]
    phone: Option<String>,
    #[serde(rename = "pylx")]
    stutype: Option<String>,
}

#[derive(Deserialize, Default)]
struct LdapInfo {
    #[serde(rename = "yhdm")]
    stu_id: Option<String>,
    #[serde(rename = "xm")]
    name: Option<String>,
}

// ── public API ──────────────────────────────────────────────────────────────

/// Three-step mjw login: bootstrap → RSA key (best-effort) → LDAP.
pub async fn mjw_login(username: &str, password: &str) -> Result<MjwSession> {
    let client = Client::builder()
        .user_agent(USER_AGENT)
        .cookie_store(true)
        .build()
        .context("failed to build reqwest client")?;

    // Step 1 — bootstrap: get route cookie
    let r1 = client
        .post(format!("{MJW_BASE}/component/queryApplicationSetting/rsa"))
        .header("Authorization", BASIC_AUTH)
        .header("rolecode", "01")
        .header("_lang", "cn")
        .body("")
        .send()
        .await
        .context("mjw bootstrap request failed")?;
    if !r1.status().is_success() {
        anyhow::bail!("mjw bootstrap returned HTTP {}", r1.status());
    }
    let _ = r1.text().await?; // consume body, cookies stored by cookie_store

    // Step 2 — RSA key (best-effort; login currently uses plaintext password)
    let _ = client
        .post(format!("{MJW_BASE}/c_raskey"))
        .header("Authorization", BASIC_AUTH)
        .header("rolecode", "06")
        .header("_lang", "cn")
        .body("")
        .send()
        .await;

    // Step 3 — LDAP login with plaintext password, rolecodes [06, 01]
    let token = try_ldap_login(&client, username, password).await?;
    Ok(MjwSession { client, token })
}

// ── internal ────────────────────────────────────────────────────────────────

async fn try_ldap_login(client: &Client, username: &str, password: &str) -> Result<MjwToken> {
    for &rc in &["06", "01"] {
        let resp = client
            .post(format!("{MJW_BASE}/authentication/ldap"))
            .header("Authorization", BASIC_AUTH)
            .header("rolecode", rc)
            .header("_lang", "cn")
            .form(&[("username", username), ("password", password)])
            .send()
            .await
            .context("mjw LDAP login request failed")?;

        let body = resp.text().await.context("failed to read LDAP response")?;

        if let Ok(ldap) = serde_json::from_str::<LdapResponse>(&body) {
            if let Some(tok) = ldap.access_token.filter(|t| !t.is_empty()) {
                let data = ldap.data.unwrap_or_default();
                let info = ldap.info.unwrap_or_default();
                return Ok(MjwToken {
                    access_token: tok,
                    refresh_token: ldap.refresh_token,
                    name: data.name.or(info.name),
                    school: data.school,
                    stu_id: info.stu_id,
                    stutype: data.stutype,
                    phone: data.phone,
                });
            }
            // Check for explicit error message
            if body.contains("用户名或密码错误") {
                anyhow::bail!("username or password incorrect");
            }
        }
    }
    anyhow::bail!("LDAP login failed for all rolecodes; check credentials")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_successful_ldap_response() {
        let json = r#"{
            "access_token":"abc123",
            "refresh_token":"ref456",
            "data":{"pylx":"1","lxdh":"1234567890","yhlx":"1","yhxm":"张三","sfjxyz":false,"bmmc":"信息学部"},
            "scope":"all",
            "token_type":"bearer",
            "expires_in":7327462,
            "info":{"yhdm":"20220001","xm":"张三","roles":["01"]}
        }"#;
        let resp: LdapResponse = serde_json::from_str(json).expect("parse");
        let tok = resp.access_token.unwrap();
        assert_eq!(tok, "abc123");
        assert_eq!(resp.refresh_token.as_deref(), Some("ref456"));
        let data = resp.data.unwrap();
        assert_eq!(data.name.as_deref(), Some("张三"));
        assert_eq!(data.school.as_deref(), Some("信息学部"));
    }
}
