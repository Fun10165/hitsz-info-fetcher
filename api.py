#!/usr/bin/env python3
"""REST API for HITSZ info portal notices.
Serves notices.json with filtering by date range and category.
Run: python3 api.py [--port 8080] [--host 0.0.0.0]
"""

import json
import os
import argparse
from datetime import datetime, date
from http.server import HTTPServer, BaseHTTPRequestHandler
from urllib.parse import urlparse, parse_qs

NOTICES_FILE = os.environ.get("NOTICES_FILE", "notices.json")


def load_notices():
    try:
        with open(NOTICES_FILE, "r", encoding="utf-8") as f:
            return json.load(f)
    except (FileNotFoundError, json.JSONDecodeError):
        return []


class NoticeAPIHandler(BaseHTTPRequestHandler):
    def _send_json(self, code, data):
        body = json.dumps(data, ensure_ascii=False, indent=2).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        parsed = urlparse(self.path)
        path = parsed.path.rstrip("/") or "/"
        qs = parse_qs(parsed.query)

        notices = load_notices()

        # ── routes ──────────────────────────────────────────────

        if path == "/" or path == "/health":
            self._send_json(200, {
                "status": "ok",
                "notices": len(notices),
                "endpoints": [
                    "GET /notices — all notices (optional ?from=&to=&category=&department=)",
                    "GET /notices/today — today's notices only",
                    "GET /notices/recent?days=N — last N days",
                    "GET /notices/<date> — notices on specific date (YYYY-MM-DD)",
                    "GET /stats — summary statistics",
                ],
            })
            return

        if path == "/notices":
            filtered = self._filter(notices, qs)
            self._send_json(200, {
                "count": len(filtered),
                "notices": filtered,
            })
            return

        if path == "/notices/today":
            today = date.today().isoformat()
            filtered = [n for n in notices if n.get("date") == today]
            self._send_json(200, {
                "date": today,
                "count": len(filtered),
                "notices": filtered,
            })
            return

        if path == "/notices/recent":
            days = int(qs.get("days", ["7"])[0])
            today = date.today()
            cutoff = today.replace(year=today.year - 1) if days > 365 else today
            from datetime import timedelta
            cutoff = today - timedelta(days=days)
            cutoff_str = cutoff.isoformat()
            filtered = [n for n in notices if n.get("date", "") >= cutoff_str]
            self._send_json(200, {
                "days": days,
                "from": cutoff_str,
                "count": len(filtered),
                "notices": filtered,
            })
            return

        if path == "/stats":
            by_date = {}
            by_category = {}
            by_department = {}
            for n in notices:
                d = n.get("date", "unknown")
                by_date[d] = by_date.get(d, 0) + 1
                c = n.get("category", "unknown")
                by_category[c] = by_category.get(c, 0) + 1
                dept = n.get("department", "unknown")
                by_department[dept] = by_department.get(dept, 0) + 1
            self._send_json(200, {
                "total": len(notices),
                "date_range": {
                    "earliest": min(by_date.keys()) if by_date else None,
                    "latest": max(by_date.keys()) if by_date else None,
                },
                "by_date": dict(sorted(by_date.items(), reverse=True)),
                "by_category": by_category,
                "by_department": by_department,
            })
            return

        # /notices/YYYY-MM-DD
        if path.startswith("/notices/"):
            date_str = path.split("/")[2]
            filtered = [n for n in notices if n.get("date") == date_str]
            if not filtered:
                self._send_json(404, {"error": f"no notices on {date_str}"})
            else:
                self._send_json(200, {
                    "date": date_str,
                    "count": len(filtered),
                    "notices": filtered,
                })
            return

        self._send_json(404, {"error": f"unknown path: {path}"})

    def _filter(self, notices, qs):
        result = notices
        if "from" in qs:
            result = [n for n in result if n.get("date", "") >= qs["from"][0]]
        if "to" in qs:
            result = [n for n in result if n.get("date", "") <= qs["to"][0]]
        if "category" in qs:
            cat = qs["category"][0]
            result = [n for n in result if cat in n.get("category", "")]
        if "department" in qs:
            dept = qs["department"][0]
            result = [n for n in result if dept in n.get("department", "")]
        if "q" in qs:
            q = qs["q"][0].lower()
            result = [n for n in result if q in n.get("title", "").lower()]
        return result

    def log_message(self, fmt, *args):
        print(f"[{datetime.now().strftime('%H:%M:%S')}] {fmt % args}")


def main():
    parser = argparse.ArgumentParser(description="HITSZ Notices REST API")
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=8080)
    args = parser.parse_args()

    notices = load_notices()
    print(f"loaded {len(notices)} notices from {NOTICES_FILE}")
    print(f"API running on http://{args.host}:{args.port}")

    server = HTTPServer((args.host, args.port), NoticeAPIHandler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nshutting down")


if __name__ == "__main__":
    main()
