# HITSZ Info Fetcher

Rust-based browser automation for fetching authenticated content from the HITSZ internal information portal.

## What it does

- authenticates against HIT unified identity in a real browser
- prefers reusing existing Chrome session state when available
- falls back to interactive browser login when needed
- fetches the latest notices list from:
  - `http://info.hitsz.edu.cn/list.jsp?wbtreeid=1053`
- outputs structured JSON with authenticated profile and fetched page data

## Safety notes

- This project does **not** ship any real credentials, cookies, or personal data.
- Provide your own credentials locally through command-line flags or environment variables.
- Do **not** commit browser debug artifacts, traces, screenshots, or session files.

## Requirements

- Rust toolchain
- Google Chrome installed locally
- macOS users may need to set linker-related environment variables when building

## Build

```bash
cargo check
```

On macOS, if linking fails with `-liconv`, build with:

```bash
SDKROOT="$(xcrun --sdk macosx --show-sdk-path)" \
LIBRARY_PATH="$(xcrun --sdk macosx --show-sdk-path)/usr/lib" \
CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER=/usr/bin/clang \
cargo build
```

## Run

With explicit credentials:

```bash
cargo run -- \
  --username "YOUR_USERNAME" \
  --password "YOUR_PASSWORD"
```

Or set environment variables and let the CLI attempt browser session reuse first:

```bash
export HITSZ_USERNAME="YOUR_USERNAME"
export HITSZ_PASSWORD="YOUR_PASSWORD"

cargo run --
```

Optional flags:

- `--info-url` override target page

Current default target:

- `http://info.hitsz.edu.cn/list.jsp?wbtreeid=1053`

## Browser session reuse

The fetcher tries these strategies in order:

1. connect to an already running Chrome debugging endpoint
2. launch Chrome with an existing user profile
3. fall back to explicit login flow

Supported environment variables:

- `HITSZ_USERNAME`
- `HITSZ_PASSWORD`
- `HITSZ_CHROME_DEBUG_URL`
- `HITSZ_CHROME_USER_DATA_DIR`
- `HITSZ_CHROME_PROFILE_DIRECTORY`
- `HITSZ_CHROME_EXECUTABLE`

## Output

The binary prints JSON similar to:

```json
{
  "token": {
    "username": "...",
    "name": "..."
  },
  "fetched_page": {
    "final_url": "http://info.hitsz.edu.cn/list.jsp?wbtreeid=1053",
    "title": "办公信息网",
    "links": []
  }
}
```

## AutoCLI wrapper

This repo can be wrapped by an external AutoCLI command, but the user-specific wrapper/config lives outside this repository and is intentionally not included here.

## Development notes

- `debug-artifacts/` is intentionally ignored because it may contain sensitive authentication pages or personal identifiers.
- `target/` is ignored as a build artifact.

## License

MIT
