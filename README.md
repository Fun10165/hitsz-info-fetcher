# HITSZ Info Fetcher

Rust-based browser automation for fetching authenticated notice content from the HITSZ internal information portal. Uses a real Chrome browser via CDP to handle HIT unified identity SSO, CAS redirects, and multifactor authentication.

## Quick Start

```bash
cargo run
```

If no credentials are provided, the CLI prompts interactively (password input is hidden):

```
Username: 2023311001
Password: 
```

By default, only **today's notices** are extracted. After authentication, the program prints structured JSON with your profile and filtered notices.

## CLI Reference

```
hitsz-info-fetcher [OPTIONS]

  --username <USERNAME>     HIT unified identity username (or stdin prompt)
  --password <PASSWORD>     HIT unified identity password (or hidden stdin prompt)
  --info-url <URL>          Target page URL [default: http://info.hitsz.edu.cn/list.jsp?wbtreeid=1053]
  --from <DATE>             Start date, YYYY-MM-DD inclusive
  --to <DATE>               End date, YYYY-MM-DD inclusive
  --today                   Shortcut: only today's notices
  --all                     Fetch all notices without date filtering
  --accept-invalid-certs    Accept invalid TLS certificates
  -h, --help                Print help
  -V, --version             Print version
```

### Date Filtering

| Command | Behavior |
|---|---|
| `cargo run` | Default: today only, auto-paginates |
| `cargo run -- --today` | Same as default, explicit |
| `cargo run -- --from 2026-06-01 --to 2026-06-16` | Date range, auto-paginates until oldest notice < `--from` |
| `cargo run -- --from 2026-06-10` | From June 10 onward |
| `cargo run -- --to 2026-06-10` | Everything up to June 10 |
| `cargo run -- --all` | No filtering, single page only |

Notices are listed newest-first. When a date filter is active, the fetcher automatically paginates through subsequent pages until the oldest notice on the current page falls before the lower bound.

### Credential Sources

Credentials are resolved in priority order:

1. CLI flags `--username` / `--password`
2. Environment variables `HITSZ_USERNAME` / `HITSZ_PASSWORD`
3. Interactive terminal prompt

## Authentication Flow

The fetcher attempts browser connection in this order:

1. **Connect to existing Chrome** — tries remote debugging endpoints on ports 9222, 9223, localhost:9222
2. **Launch Chrome with user profile** — reuses your existing Chrome profile (preserves login cookies)
3. **Launch isolated Chrome** — clean session, falls back to explicit login

Once a browser session is established:

| Scenario | Behavior |
|---|---|
| Already on JW portal | Skip login, proceed to fetch |
| Cached cookies → 2FA page | Skip credential form, prompt for verification code in terminal |
| Fresh login needed | Fill username/password in browser form → handle 2FA if triggered |

**Multifactor authentication**: when a 2FA page is detected, the program:

1. Clicks "get verification code" in the browser
2. Prompts `Enter verification code:` in the terminal
3. Fills the code and clicks submit automatically
4. Waits for SSO redirect to complete

Timeout for login completion: **5 minutes**.

## Output

Standard output is pretty-printed JSON:

```json
{
  "token": {
    "username": "2023311001",
    "name": "王五",
    "stutype": "undergrad",
    "stu_id": "2023311001",
    "school": "信息学部",
    "major": "工科试验班（计算机与电子通信）",
    "grade": "2023",
    "email": "ENC:...",
    "phone": "ENC:..."
  },
  "fetched_page": {
    "final_url": "http://info.hitsz.edu.cn/list.jsp?wbtreeid=1053",
    "title": "办公信息网",
    "links": [...]
  },
  "today_notices": {
    "notices": [
      {
        "title": "关于启动2026/2027学年跨校区流动学习项目预报名工作的通知",
        "url": "http://info.hitsz.edu.cn/content.jsp?...&wbnewsid=9219",
        "date": "2026-06-16",
        "department": "教务部",
        "category": "【工作通知】"
      }
    ],
    "pages_fetched": 1
  }
}
```

- `token` — authenticated user profile. `password`, `email`, `phone` are encrypted and marked `#[serde(skip_serializing)]` or base64-encoded.
- `fetched_page` — raw page snapshot (all links, full HTML). Always present.
- `today_notices` — structured notice items with date/department/category. Present when date filtering is active; `null` with `--all`.

## Environment Variables

| Variable | Purpose |
|---|---|
| `HITSZ_USERNAME` | Default username (overridden by `--username`) |
| `HITSZ_PASSWORD` | Default password (overridden by `--password`) |
| `HITSZ_CHROME_DEBUG_URL` | Custom remote debugging endpoint |
| `HITSZ_CHROME_USER_DATA_DIR` | Chrome user data directory path |
| `HITSZ_CHROME_PROFILE_DIRECTORY` | Chrome profile subdirectory (default: `Default`) |
| `HITSZ_CHROME_EXECUTABLE` | Chrome binary path |
| `PERSIST_DEBUG_ARTIFACTS` | Set to `1` to save screenshots/HTML per navigation step |

## Architecture

```
main.rs (CLI, clap)
  └─> browser_auth.rs  (primary auth + fetch path)
        ├── chromiumoxide (Chrome CDP)
        ├── SSO login + 2FA handling
        ├── Profile fetch via JW POST API
        ├── Notice extraction with date filtering + pagination
        └── extract.rs  (scraper-based HTML parsing)
              ├── extract_page_snapshot()  — generic page metadata
              ├── extract_notice_items()   — structured notice parsing
              └── find_next_page_url()     — pagination link detection

auth.rs (alternate reqwest CAS path — not wired in main.rs)
models.rs (EasToken, NoticeItem, PageSnapshot, etc.)
```

## Build

Requires Rust toolchain and Google Chrome.

```bash
cargo build
```

On macOS, if linking fails with `-liconv`:

```bash
LIBRARY_PATH="$(brew --prefix)/lib" cargo build
```

## Development

```bash
cargo test          # 6 tests covering extract.rs, auth.rs
cargo build         # debug build
```

- Edition: Rust 2024
- Async runtime: tokio (multi-threaded)
- Browser automation: chromiumoxide 0.9
- HTML parsing: scraper (CSS selectors)
- `debug-artifacts/` is gitignored (may contain sensitive page content)
- `banks/` is gitignored (harness-internal)

## Security

- This project does **not** ship credentials, cookies, or personal data.
- Provide your own credentials via CLI flags, environment variables, or interactive prompt.
- Email and phone fields in the output are encrypted (AES, base64-encoded strings).
- Do not commit `debug-artifacts/`, `.env`, screenshots, or browser traces.

## License

MIT
