use anyhow::{Context, Result};
use chrono::Local;
use clap::Parser;
use hitsz_info_fetcher::browser_auth::login_and_fetch_info_via_browser;
use std::io::{self, Write};

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about = "Authenticate with HIT unified identity and fetch authenticated page content"
)]
struct Cli {
    #[arg(long)]
    username: Option<String>,
    #[arg(long)]
    password: Option<String>,
    #[arg(long, default_value = "http://info.hitsz.edu.cn/list.jsp?wbtreeid=1053")]
    info_url: String,
    /// Start date for notice filtering (YYYY-MM-DD, inclusive)
    #[arg(long)]
    from: Option<String>,
    /// End date for notice filtering (YYYY-MM-DD, inclusive)
    #[arg(long)]
    to: Option<String>,
    /// Shortcut: only today's notices (equivalent to --from today --to today)
    #[arg(long, default_value_t = false)]
    today: bool,
    /// Fetch all notices without date filtering
    #[arg(long, default_value_t = false)]
    all: bool,
    #[arg(long, default_value_t = true)]
    accept_invalid_certs: bool,
}

fn resolve_username(cli: &Cli) -> Result<String> {
    if let Some(u) = &cli.username {
        if !u.is_empty() {
            return Ok(u.clone());
        }
    }
    if let Ok(u) = std::env::var("HITSZ_USERNAME") {
        if !u.is_empty() {
            return Ok(u);
        }
    }
    print!("Username: ");
    io::stdout().flush().context("failed to flush stdout")?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read username from stdin")?;
    let trimmed = input.trim().to_owned();
    if trimmed.is_empty() {
        anyhow::bail!("username is required");
    }
    Ok(trimmed)
}

fn resolve_password(cli: &Cli) -> Result<String> {
    if let Some(p) = &cli.password {
        if !p.is_empty() {
            return Ok(p.clone());
        }
    }
    if let Ok(p) = std::env::var("HITSZ_PASSWORD") {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    let password = rpassword::prompt_password("Password: ")
        .context("failed to read password from stdin")?;
    if password.is_empty() {
        anyhow::bail!("password is required");
    }
    Ok(password)
}

fn resolve_date_range(cli: &Cli) -> (Option<String>, Option<String>) {
    if cli.all {
        return (None, None);
    }
    // Default to today when no date flags are given
    let today_str = || Local::now().format("%Y-%m-%d").to_string();
    if cli.today || (cli.from.is_none() && cli.to.is_none()) {
        let t = today_str();
        return (Some(t.clone()), Some(t));
    }
    (cli.from.clone(), cli.to.clone())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let _ = cli.accept_invalid_certs;
    let username = resolve_username(&cli)?;
    let password = resolve_password(&cli)?;
    let (date_from, date_to) = resolve_date_range(&cli);
    let result = login_and_fetch_info_via_browser(
        Some(&username),
        Some(&password),
        Some(&cli.info_url),
        date_from.as_deref(),
        date_to.as_deref(),
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
