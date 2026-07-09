use std::process;
use anyhow::{Context, Result};

#[tokio::main]
async fn main() {
    if let Err(e) = check().await {
        println!("DEAD");
        eprintln!("error: {e:#}");
        process::exit(1);
    }
}

async fn check() -> Result<()> {
    let resp = reqwest::get("http://10.249.8.100:8080/health")
        .await
        .context("failed to connect to kp3")?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        anyhow::bail!("HTTP {status}: {body:.100}");
    }

    let v: serde_json::Value = serde_json::from_str(&body)
        .context("invalid JSON response")?;

    let api_status = v["status"].as_str().unwrap_or("unknown");
    let notices = v["notices"].as_u64().unwrap_or(0);

    if api_status != "ok" {
        anyhow::bail!("status={api_status}, notices={notices}");
    }

    println!("OK notices={notices}");
    Ok(())
}
