use anyhow::{Context, Result};
use chrono::{Duration, Local};
use clap::{Args, Parser, Subcommand};
use hitsz_info_fetcher::browser_auth::login_and_fetch_info_via_browser;
use hitsz_info_fetcher::models::NoticeItem;
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::Path;

// ── CLI ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about = "Authenticate with HIT unified identity and fetch authenticated page content"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    // ── shared flags ──
    #[arg(long, global = true)]
    username: Option<String>,
    #[arg(long, global = true)]
    password: Option<String>,
    #[arg(long, default_value = "http://info.hitsz.edu.cn/list.jsp?wbtreeid=1053", global = true)]
    info_url: String,
    #[arg(long, default_value_t = true, global = true)]
    accept_invalid_certs: bool,

    // ── fetch-mode flags (ignored when a subcommand is used) ──
    #[arg(long)]
    from: Option<String>,
    #[arg(long)]
    to: Option<String>,
    #[arg(long, default_value_t = false)]
    today: bool,
    #[arg(long, default_value_t = false)]
    all: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Fetch, deduplicate, and accumulate notices into a JSON file
    Cron(CronArgs),
}

#[derive(Debug, Args)]
struct CronArgs {
    /// Path to the notices JSON file
    #[arg(long, default_value = "notices.json")]
    output: String,
    /// Number of days back to fetch (1 = today only)
    #[arg(long, default_value_t = 1)]
    days: u32,
}

// ── credential resolution ───────────────────────────────────────────────────

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
    let today_str = || Local::now().format("%Y-%m-%d").to_string();
    if cli.today || (cli.from.is_none() && cli.to.is_none()) {
        let t = today_str();
        return (Some(t.clone()), Some(t));
    }
    (cli.from.clone(), cli.to.clone())
}

// ── cron logic ──────────────────────────────────────────────────────────────

fn load_notices(path: &str) -> Result<Vec<NoticeItem>> {
    if !Path::new(path).exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {path}"))?;
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {path}"))
}

fn save_notices(path: &str, notices: &[NoticeItem]) -> Result<()> {
    let json = serde_json::to_string_pretty(notices)?;
    std::fs::write(path, json).with_context(|| format!("failed to write {path}"))?;
    Ok(())
}

async fn run_cron(cli: &Cli, args: &CronArgs) -> Result<()> {
    let today = Local::now().date_naive();
    let from = today - Duration::days(args.days as i64 - 1);
    let from_str = from.format("%Y-%m-%d").to_string();
    let to_str = today.format("%Y-%m-%d").to_string();

    eprintln!("cron: fetching notices from {} to {}", from_str, to_str);

    // Try saved cookies first (no browser needed)
    let cookie_file = "session-cookies.json";
    let result = match hitsz_info_fetcher::browser_auth::fetch_info_with_saved_cookies(
        cookie_file,
        &cli.info_url,
        Some(&from_str),
        Some(&to_str),
    )
    .await
    {
        Ok(Some(r)) => {
            eprintln!("cron: using saved session cookies");
            r
        }
        Ok(None) => {
            eprintln!("cron: saved cookies expired, falling back to browser login");
            let username = cli.username.as_deref()
                .filter(|u| !u.is_empty())
                .map(String::from)
                .or_else(|| std::env::var("HITSZ_USERNAME").ok().filter(|u| !u.is_empty()))
                .context("cron: cookies expired, need --username for browser login")?;
            let password = cli.password.as_deref()
                .filter(|p| !p.is_empty())
                .map(String::from)
                .or_else(|| std::env::var("HITSZ_PASSWORD").ok().filter(|p| !p.is_empty()))
                .context("cron: cookies expired, need --password for browser login")?;

            login_and_fetch_info_via_browser(
                Some(&username),
                Some(&password),
                Some(&cli.info_url),
                Some(&from_str),
                Some(&to_str),
                false,
            )
            .await?
        }
        Err(e) => {
            eprintln!("cron: cookie load failed ({e:#}), falling back to browser login");
            let username = cli.username.as_deref()
                .filter(|u| !u.is_empty())
                .map(String::from)
                .or_else(|| std::env::var("HITSZ_USERNAME").ok().filter(|u| !u.is_empty()))
                .context("cron: need --username or HITSZ_USERNAME")?;
            let password = cli.password.as_deref()
                .filter(|p| !p.is_empty())
                .map(String::from)
                .or_else(|| std::env::var("HITSZ_PASSWORD").ok().filter(|p| !p.is_empty()))
                .context("cron: need --password or HITSZ_PASSWORD")?;

            login_and_fetch_info_via_browser(
                Some(&username),
                Some(&password),
                Some(&cli.info_url),
                Some(&from_str),
                Some(&to_str),
                false,
            )
            .await?
        }
    };

    let new_notices = result
        .today_notices
        .map(|nl| nl.notices)
        .unwrap_or_default();

    if new_notices.is_empty() {
        eprintln!("cron: no notices in range, nothing to do");
        return Ok(());
    }

    let mut all = load_notices(&args.output)?;

    let existing_urls: HashSet<String> = all.iter().map(|n| n.url.clone()).collect();

    let mut added = 0usize;
    // new_notices are newest-first from the fetcher; prepend to keep chronological order
    for notice in new_notices.into_iter().rev() {
        if !existing_urls.contains(&notice.url) {
            all.insert(0, notice);
            added += 1;
        }
    }
    save_notices(&args.output, &all)?;
    eprintln!(
        "cron: {} new notice(s) added, {} total in {}",
        added,
        all.len(),
        args.output
    );
    Ok(())
}

// ── entry point ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let _ = cli.accept_invalid_certs;

    match &cli.command {
        Some(Command::Cron(args)) => run_cron(&cli, args).await,
        None => {
            let username = resolve_username(&cli)?;
            let password = resolve_password(&cli)?;
            let (date_from, date_to) = resolve_date_range(&cli);
            let result = login_and_fetch_info_via_browser(
                Some(&username),
                Some(&password),
                Some(&cli.info_url),
                date_from.as_deref(),
                date_to.as_deref(),
                true,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
    }
}
