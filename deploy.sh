#!/bin/bash
# Deploy HITSZ info fetcher to kp3
# 1. Copy cookie file + scripts
# 2. Set up crontab for daily fetching
# 3. Start REST API as background service

set -e
REMOTE="kp3"
REMOTE_DIR="/home/fun10165/hitsz-info"

echo "=== Creating remote directory ==="
ssh $REMOTE "mkdir -p $REMOTE_DIR"

echo "=== Copying files ==="
scp session-cookies.json $REMOTE:$REMOTE_DIR/
scp cron_fetch.py $REMOTE:$REMOTE_DIR/
scp api.py $REMOTE:$REMOTE_DIR/

echo "=== Initial fetch (365 days) ==="
ssh $REMOTE "cd $REMOTE_DIR && python3 cron_fetch.py --days 365 --output notices.json"

echo "=== Testing API ==="
ssh $REMOTE "cd $REMOTE_DIR && python3 api.py --port 8080 &" 
sleep 2
ssh $REMOTE "curl -s http://localhost:8080/health" 
ssh $REMOTE "kill %1 2>/dev/null" || true

echo "=== Setting up crontab ==="
# Run cron at 8:00, 12:00, 16:00, 20:00 daily
# Restart API at reboot
ssh $REMOTE "cat > $REMOTE_DIR/crontab.txt << 'EOF'
# HITSZ info fetcher
0 8,12,16,20 * * * cd $REMOTE_DIR && python3 cron_fetch.py --days 365 >> cron.log 2>&1
@reboot cd $REMOTE_DIR && nohup python3 api.py --port 8080 >> api.log 2>&1 &
EOF
(crontab -l 2>/dev/null | grep -v 'hitsz-info' ; cat $REMOTE_DIR/crontab.txt) | crontab -
crontab -l | grep hitsz-info
"

echo "=== Starting API now ==="
ssh $REMOTE "cd $REMOTE_DIR && nohup python3 api.py --port 8080 >> api.log 2>&1 &"
sleep 2
echo "=== API status ==="
ssh $REMOTE "curl -s http://localhost:8080/health | python3 -m json.tool"

echo ""
echo "=== Deploy complete ==="
echo "API: http://kp3:8080"
echo "Endpoints:"
echo "  GET /health"
echo "  GET /notices"
echo "  GET /notices/today"
echo "  GET /notices/recent?days=7"
echo "  GET /notices/2026-06-24"
echo "  GET /stats"
echo "  GET /notices?from=2026-06-01&to=2026-06-24"
echo "  GET /notices?category=工作通知"
echo "  GET /notices?q=奖学金"
