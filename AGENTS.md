# Repository Guidelines

## Project Overview

Rust-based browser automation CLI that authenticates against the HIT (Harbin Institute of Technology, Shenzhen) unified identity system, then fetches and extracts content from the campus information portal (`info.hitsz.edu.cn`). Uses a real Chrome browser via CDP to handle SSO redirects, CAS login, and session cookies that reqwest-alone cannot replicate.

## Architecture & Data Flow

```
main.rs (CLI entry, clap)
  └─> browser_auth.rs  (primary auth + fetch path)
        ├── Launches/connects to Chrome (chromiumoxide)
        ├── Fills credentials, handles 2FA, waits for SSO completion
        ├── Fetches target info page content
        ├── Calls build_browser_token() to construct EasToken from cookies + profile JSON
        └── Calls extract::extract_page_snapshot() to parse HTML into PageSnapshot
              └─> extract.rs  (scraper-based HTML extraction)

auth.rs (alternate reqwest-based CAS path — NOT wired in main.rs)
  └── Uses reqwest blocking client with cookie store
      to follow the CAS/IDS OAuth flow entirely over HTTP
```

**Data types** live in `models.rs`:
- `AuthenticatedFetchResult { token: EasToken, fetched_page: PageSnapshot }` — the top-level output
- `EasToken` — authenticated user profile (cookies, name, student ID, school, major, etc.)
- `PageSnapshot` — parsed HTML page: URL, title, extracted `<a>` links, raw HTML
- `ExtractedLink { title, url }` — absolute-resolved link
- `StudentType::Undergrad | Grad` — mapped from profile field `PYLX`

`EasToken::password` is **not serialized** (`#[serde(skip_serializing)]`).

## Key Directories

| Path | Purpose |
|---|---|
| `src/` | All Rust source; no subdirectories |
| `debug-artifacts/` | Gitignored; runtime debug dumps (screenshots, HTML snapshots) |
| `banks/` | Possibly runtime artifact storage; not part of compiled code |

## Development Commands

| Command | What it does |
|---|---|
| `cargo build` | Compiles the binary |
| `cargo test` | Runs unit tests (`extract.rs`, `auth.rs`) |
| `cargo run -- --username <u> --password <p>` | Fetches authenticated content |
| `cargo run -- --info-url <url>` | Override target URL |
| `cargo run -- --accept-invalid-certs=false` | Disable invalid cert acceptance (default true) |

On macOS, if linking fails with `-liconv`:
```bash
LIBRARY_PATH="$(brew --prefix)/lib" cargo build
```

## Code Conventions & Common Patterns

- **Edition 2024** — uses Rust 2024 edition features
- **Async runtime**: `tokio` (multi-threaded, `#[tokio::main]`)
- **Error handling**: `anyhow::Result` throughout; `.context()` for enrichment; `bail!()` for early exits
- **Error recovery pattern in browser_auth.rs**: `is_missing_context_error()` checks for the error message `"Cannot find context with specified id"` — used to retry browser connection when Chrome tab contexts disappear
- **Fallback chains**: Browser connection tries existing Chrome first (`try_connect_existing_browser()`), then profile launch (`try_launch_browser_with_profile()`), then clean launch (`launch_browser()`)
- **Credential resolution**: `username`/`password` parameters (optional `&str`) fall back to env vars `HITSZ_USERNAME` / `HITSZ_PASSWORD`
- **Chrome discovery**: `chrome_user_data_dir()` checks `CHROME_USER_DATA_DIR` env var, then falls back to platform defaults (macOS: `~/Library/Application Support/Google/Chrome`; Linux: `~/.config/google-chrome`; Windows: `%LOCALAPPDATA%\Google\Chrome\User Data`)
- **Selector-based UI automation**: Uses CSS selectors (`input#username`, `input#password`, login buttons by text) to fill forms; `wait_for_selector()` polls with timeout
- **2FA support**: Detects two-factor prompt, pauses for interactive input (reads from stdin in `request_two_factor_code()`)
- **Debug artifacts**: `persist_page_debug_artifacts()` saves screenshots and HTML to `debug-artifacts/` with timestamped filenames; controlled by `PERSIST_DEBUG_ARTIFACTS` env var
- **HTML extraction**: `extract_page_snapshot()` uses `scraper` crate (CSS selectors); resolves relative links via `Url::join()`
- **Viewport**: 1600×1400 (hardcoded in `launch_browser()`)
- **Constant URL bases**: All HIT portal URLs are `const &str` at module top (`INFO_DEFAULT_URL`, `JW_CAS_URL`, `JW_HOST`, etc.)

## Important Files

| File | Role |
|---|---|
| `src/main.rs` | Binary entry point; clap CLI; calls `login_and_fetch_info_via_browser` |
| `src/lib.rs` | Library root; re-exports all modules |
| `src/models.rs` | All data types (`EasToken`, `PageSnapshot`, `AuthenticatedFetchResult`, etc.) |
| `src/browser_auth.rs` | Primary module (~620 lines): Chrome browser automation via `chromiumoxide` (CDP) |
| `src/auth.rs` | Alternate module (~345 lines): pure HTTP CAS/OAuth flow via `reqwest` (not currently used by `main.rs`) |
| `src/extract.rs` | HTML extraction: parses title + links via `scraper` |
| `Cargo.toml` | Dependencies: `clap` (derive), `chromiumoxide`, `reqwest` (rustls-tls, cookies), `scraper`, `serde`/`serde_json`, `tokio`, `url`, `anyhow`, `futures-util` |

## Runtime/Tooling Preferences

- **Rust toolchain**: latest stable (edition 2024)
- **Chrome**: must be installed locally; `DEFAULT_CHROME_EXECUTABLE` hardcoded to macOS path (`/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`)
- **No CI/CD config present** in repository — test/format/lint locally only
- **`.gitignore`**: excludes `target/`, `debug-artifacts/`, `.env*`, browser artifacts (`*.har`, `*.trace`, `screenshots/`), editor files

## Testing & QA

- **Framework**: Rust built-in `#[test]` / `#[cfg(test)] mod tests`
- **Run**: `cargo test`
- **Covered areas**:
  - `extract.rs` — tests `extract_page_snapshot()` for title + link extraction and absolute URL resolution
  - `auth.rs` — tests `parse_auth_form()` (hidden field parsing) and `build_eas_token()` (profile JSON → `EasToken`)
- **No tests for browser_auth.rs** — Chrome-dependent; not unit-testable without a running browser
- **Debug mode**: set `PERSIST_DEBUG_ARTIFACTS=1` to save screenshots and page HTML per navigation step, and/or `RUST_LOG=debug` for crate-level logging (if tracing subscriber is wired)
