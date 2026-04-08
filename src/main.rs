use anyhow::Result;
use clap::Parser;
use hitsz_info_fetcher::browser_auth::login_and_fetch_info_via_browser;

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
    #[arg(long, default_value_t = true)]
    accept_invalid_certs: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let _ = cli.accept_invalid_certs;
    let result = login_and_fetch_info_via_browser(
        cli.username.as_deref(),
        cli.password.as_deref(),
        Some(&cli.info_url),
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
