#!/bin/bash
# Auto-refresh HITSZ session cookies for kp3 cron
# Reads credentials from env vars or ~/.hitsz-env

cd "$(dirname "$0")"
export PATH="$HOME/.nix-profile/bin:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:$PATH"

[ -f "$HOME/.hitsz-env" ] && source "$HOME/.hitsz-env"

USERNAME="${HITSZ_USERNAME:-}"
PASSWORD="${HITSZ_PASSWORD:-}"

if [ -z "$USERNAME" ] || [ -z "$PASSWORD" ]; then
    echo "[$(date)] ERROR: HITSZ_USERNAME or HITSZ_PASSWORD not set" >&2
    exit 1
fi

echo "[$(date)] Starting browser login..."
cargo run --release -- --username "$USERNAME" --password "$PASSWORD"
EXIT=$?

if [ $EXIT -ne 0 ]; then
    echo "[$(date)] Login failed (exit $EXIT)" >&2
    exit $EXIT
fi

if [ ! -f session-cookies.json ]; then
    echo "[$(date)] No session-cookies.json generated" >&2
    exit 1
fi

echo "[$(date)] Syncing to kp3..."
scp session-cookies.json kp3:/home/fun10165/hitsz-info/
echo "[$(date)] Done"
