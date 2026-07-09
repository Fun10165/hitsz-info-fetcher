#!/bin/bash
# Run browser login and sync cookies to kp3
set -e

echo "=== Running browser login ==="
cargo run -- "$@"
EXIT=$?

if [ $EXIT -ne 0 ]; then
    echo "Login failed (exit $EXIT), not syncing"
    exit $EXIT
fi

if [ ! -f session-cookies.json ]; then
    echo "No session-cookies.json generated"
    exit 1
fi

echo "=== Syncing cookies to kp3 ==="
scp session-cookies.json kp3:/home/fun10165/hitsz-info/
echo "=== Done ==="
