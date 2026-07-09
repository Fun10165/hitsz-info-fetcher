#!/usr/bin/env python3
"""Cron script: fetch notices via saved cookies, deduplicate, append to notices.json.
Designed to run on kp3 without Rust/browser — pure Python HTTP.

Usage:
  python3 cron_fetch.py [--days 365] [--cookie-file session-cookies.json] [--output notices.json]

Requires: session-cookies.json (from browser login on local machine)
"""

import json
import os
import re
import sys
import argparse
from datetime import datetime, date, timedelta
from urllib.request import Request, urlopen, HTTPCookieProcessor, build_opener, HTTPRedirectHandler
from urllib.error import HTTPError
import urllib.request
import time
from urllib.parse import urljoin

INFO_URL = "http://info.hitsz.edu.cn/list.jsp?wbtreeid=1053"


def load_cookies_as_dict(path):
    """Load cookies from our JSON format (name, value, domain, path)."""
    with open(path, "r") as f:
        cookies = json.load(f)
    return cookies


def build_cookie_header(cookies, target_host):
    """Build Cookie header — include all hit.edu.cn domain cookies
    so CAS redirect to ids.hit.edu.cn gets CASTGC."""
    pairs = []
    for c in cookies:
        domain = c.get("domain", "")
        if (domain in target_host or target_host in domain
                or domain.endswith("hit.edu.cn")
                or domain.endswith("hitsz.edu.cn")
                or not domain):
            pairs.append(f"{c['name']}={c['value']}")
    return "; ".join(pairs)

def http_get(url, cookies, jar=None, _depth=0):
    """Fetch URL with cookies + cookie jar, manually following redirects.
    The jar stores Set-Cookie from each response so CAS session cookies
    (JSESSIONID for info domain) persist across redirects."""
    from urllib.parse import urlparse
    from http.cookiejar import CookieJar

    if jar is None:
        jar = CookieJar()

    # Pre-load saved cookies into the jar
    if _depth == 0:
        for c in cookies:
            domain = c.get("domain", "")
            host = urlparse(url).hostname or ""
            # Only inject cookies that match the initial domain chain
            if (domain in host or host in domain
                    or domain.endswith("hit.edu.cn")
                    or not domain):
                jar.set_cookie(_make_cookie(c, url))

    opener = build_opener(HTTPCookieProcessor(jar), _NoRedirect())

    req = Request(url, headers={
        "User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
    })

    try:
        resp = opener.open(req, timeout=15)
        return resp.url, resp.read().decode("utf-8", errors="replace")
    except HTTPError as e:
        if e.code in (301, 302, 303, 307, 308) and _depth < 10:
            loc = e.headers.get("Location", "")
            if loc:
                return http_get(urljoin(url, loc), cookies, jar, _depth + 1)
        # Explicit error — die so user can handle it
        raise RuntimeError(
            f"HTTP {e.code} at {url}"
        ) from e
    except Exception as e:
        raise RuntimeError(f"fetch failed: {url}: {e}") from e


def _make_cookie(saved, url):
    """Create a http.cookiejar.Cookie from our saved format."""
    from http.cookiejar import Cookie
    from urllib.parse import urlparse
    import time
    domain = saved.get("domain", urlparse(url).hostname or "")
    if domain.startswith("."):
        domain = domain[1:]
    return Cookie(
        version=0, name=saved["name"], value=saved["value"],
        port=None, port_specified=False,
        domain=domain, domain_specified=True, domain_initial_dot=False,
        path=saved.get("path", "/"), path_specified=True,
        secure=False, expires=None, discard=False,
        comment=None, comment_url=None, rest={}, rfc2109=False,
    )


class _NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def extract_js_redirect(html):
    """Parse window.location.href='...' from HTML."""
    m = re.search(r"window\.location\.href=['\"]([^'\"]+)['\"]", html)
    return m.group(1) if m else None


def extract_notices(html, base_url):
    """Parse notice items from the info portal list page HTML."""
    notices = []
    # Each notice: <li ...><span>部门   YYYY-MM-DD</span>【分类】<a href="...">标题</a></li>
    li_pattern = re.compile(
        r'<li[^>]*>\s*<span[^>]*>(.*?)</span>\s*(【[^】]*】)?\s*<a\s+[^>]*href="([^"]+)"[^>]*>(.*?)</a>',
        re.DOTALL
    )
    for m in li_pattern.finditer(html):
        span_text = re.sub(r'<[^>]+>', '', m.group(1)).strip()
        category = (m.group(2) or "").strip()
        href = m.group(3)
        title = re.sub(r'<[^>]+>', '', m.group(4)).strip()

        # Split span: "部门   YYYY-MM-DD"
        date_match = re.search(r'(\d{4}-\d{2}-\d{2})', span_text)
        if not date_match:
            continue
        notice_date = date_match.group(1)
        department = span_text[:date_match.start()].strip().replace('\xa0', '').strip()

        full_url = urljoin(base_url, href.replace("&amp;", "&"))
        notices.append({
            "title": title,
            "url": full_url,
            "date": notice_date,
            "department": department,
            "category": category,
        })
    return notices


def find_next_page_url(html, base_url):
    """Find the '下页' (next page) link."""
    m = re.search(r'<a\s+href="([^"]+)"[^>]*>\s*下页\s*</a>', html)
    if m:
        return urljoin(base_url, m.group(1).replace("&amp;", "&"))
    return None


def fetch_notice_content(url, cookies, jar):
    """Fetch a notice content page, following JS redirects.
    Returns (final_url, html)."""
    final_url, html = http_get(url, cookies, jar)
    hops = 0
    while html and "vsb_content" not in html and hops < 3:
        js_url = extract_js_redirect(html)
        if not js_url:
            break
        final_url, html = http_get(js_url, cookies, jar)
        hops += 1
    return final_url, html


def extract_attachments(html, base_url):
    """Extract attachment download links from a notice content page.
    Returns list of (filename, download_url)."""
    attachments = []
    # Pattern: <a href="/system/_content/download.jsp?urltype=news.DownloadAttachUrl&...&wbfileid=XXX">filename</a>
    pattern = re.compile(
        r'<a\s+[^>]*href="([^"]*download\.jsp[^"]*)"[^>]*>(.*?)</a>',
        re.DOTALL
    )
    for m in pattern.finditer(html):
        href = m.group(1).replace("&amp;", "&")
        name = re.sub(r'<[^>]+>', '', m.group(2)).strip()
        if not name:
            name = "attachment"
        full_url = urljoin(base_url, href)
        attachments.append((name, full_url))
    return attachments


def extract_embedded_media(html, base_url):
    """Extract embedded PDFs/images from content that aren't attachment links.
    Returns list of (filename, download_url)."""
    media = []
    # <embed src="..."> / <object data="..."> / <iframe src="...">
    for tag, attr in [("embed", "src"), ("object", "data"), ("iframe", "src")]:
        for m in re.finditer(rf'<{tag}\s+[^>]*{attr}="([^"]+)"', html, re.DOTALL):
            src = m.group(1).replace("&amp;", "&")
            if src.startswith("http") or src.startswith("/"):
                full_url = urljoin(base_url, src)
                name = os.path.basename(src.split("?")[0]) or "embedded"
                media.append((name, full_url))
    # <img src="..."> — only if it looks like a document scan (not icon/button)
    for m in re.finditer(r'<img\s+[^>]*src="([^"]+)"[^>]*>', html, re.DOTALL):
        src = m.group(1).replace("&amp;", "&")
        if src.startswith("http") or src.startswith("/"):
            # Skip tiny icons (heuristic: check width/height attrs)
            tag_text = m.group(0)
            if re.search(r'width=["\']([12]\d|0)\d', tag_text):
                continue  # skip small images (<30px)
            full_url = urljoin(base_url, src)
            name = os.path.basename(src.split("?")[0]) or "image"
            media.append((name, full_url))
    return media


def download_file(url, cookies, jar, save_path, referer=None):
    """Download a binary file (not used in snapshot — attachments require captcha)."""
    return False, "captcha required (manual download)", 0


def snapshot_notice(notice, cookies, jar, base_dir="snapshots"):
    """Fetch notice content page, save HTML snapshot + download attachments."""
    url = notice["url"]
    m = re.search(r'wbnewsid=(\d+)', url)
    news_id = m.group(1) if m else str(abs(hash(url)))
    snap_dir = os.path.join(base_dir, f"wbnewsid_{news_id}")
    att_dir = os.path.join(snap_dir, "attachments")
    meta_path = os.path.join(snap_dir, "metadata.json")
    if os.path.exists(meta_path):
        return {"skipped": True, "dir": snap_dir}

    os.makedirs(att_dir, exist_ok=True)

    # Fetch content page
    final_url, html = fetch_notice_content(url, cookies, jar)

    if not html or "vsb_content" not in html:
        if "authserver/login" in html or "caslogin" in html:
            return {"error": "cookies expired", "dir": snap_dir}
        return {"error": f"no content (len={len(html)})", "dir": snap_dir}

    # Save HTML snapshot
    html_path = os.path.join(snap_dir, "content.html")
    with open(html_path, "w", encoding="utf-8") as f:
        f.write(html)

    # Extract and download attachments
    attachments = extract_attachments(html, final_url)
    embedded = extract_embedded_media(html, final_url)
    all_files = attachments + embedded
    # Record attachment URLs without downloading (server requires captcha)
    downloaded = [
        {"name": filename, "url": file_url, "error": "captcha required (manual download)"}
        for filename, file_url in all_files
    ]

    # Save metadata
    meta = {
        "url": url,
        "title": notice.get("title", ""),
        "date": notice.get("date", ""),
        "department": notice.get("department", ""),
        "category": notice.get("category", ""),
        "snapshot_at": datetime.now().isoformat(),
        "content_html": "content.html",
        "attachments": downloaded,
    }
    with open(meta_path, "w", encoding="utf-8") as f:
        json.dump(meta, f, ensure_ascii=False, indent=2)

    return {"dir": snap_dir, "attachments": len(downloaded), "html_size": len(html)}


def load_existing(path):
    if os.path.exists(path):
        with open(path, "r", encoding="utf-8") as f:
            try:
                return json.load(f)
            except json.JSONDecodeError:
                return []
    return []


def save(path, notices):
    with open(path, "w", encoding="utf-8") as f:
        json.dump(notices, f, ensure_ascii=False, indent=2)


SECTIONS_ALL = "1023,1024,1025,1026,1027,1083,1028,1029,1030,1031,1032,1033,1034,1035,1036,1037,1038,1039,1040,1041,1042,1043,1044,1053"

def main():
    parser = argparse.ArgumentParser(description="Cron fetch HITSZ notices from multiple sections")
    parser.add_argument("--days", type=int, default=0,
                        help="days back (0 = auto: from oldest existing notice date)")
    parser.add_argument("--full", action="store_true",
                        help="fetch everything (ignore existing data)")
    parser.add_argument("--cookie-file", default="session-cookies.json")
    parser.add_argument("--output", default="notices.json")
    parser.add_argument("--sections", default=SECTIONS_ALL,
                        help="comma-separated wbtreeid list (default: all 24 sections)")
    args = parser.parse_args()
    if not os.path.exists(args.cookie_file):
        print(f"error: cookie file {args.cookie_file} not found", file=sys.stderr)
        print("run browser login first to generate session-cookies.json", file=sys.stderr)
        sys.exit(1)

    cookies = load_cookies_as_dict(args.cookie_file)
    from http.cookiejar import CookieJar
    jar = CookieJar()

    section_ids = args.sections.split(",")
    section_ids = [s.strip() for s in section_ids if s.strip()]

    # Determine from date
    today = date.today()
    if args.full:
        from_str = "2020-01-01"
    elif args.days > 0:
        from_str = (today - timedelta(days=args.days - 1)).isoformat()
    else:
        # Auto: use oldest existing notice date, or default to 365 days
        existing = load_existing(args.output) if os.path.exists(args.output) else []
        if existing:
            oldest = min(n["date"] for n in existing if n.get("date"))
            from_str = oldest
            print(f"cron: resuming from oldest existing date: {oldest}")
        else:
            from_str = (today - timedelta(days=364)).isoformat()
    to_str = today.isoformat()

    # Fetch from each section
    all_notices = []
    total_pages = 0
    for wid in section_ids:
        info_url = f"http://info.hitsz.edu.cn/list.jsp?wbtreeid={wid}"
        notices, pages = fetch_section(info_url, cookies, jar, from_str)
        all_notices.extend(notices)
        total_pages += pages
        print(f"  section {wid}: {len(notices)} notices, {pages} pages")

    all_notices = [n for n in all_notices if from_str <= n.get("date", "") <= to_str]
    print(f"total: {len(all_notices)} notices from {total_pages} pages ({len(section_ids)} sections)")

    # Deduplicate and merge
    existing = load_existing(args.output)
    existing_urls = {n["url"] for n in existing}

    added = 0
    new_notices = []
    for notice in reversed(all_notices):
        if notice["url"] not in existing_urls:
            existing.insert(0, notice)
            existing_urls.add(notice["url"])
            new_notices.append(notice)
            added += 1

    save(args.output, existing)
    print(f"cron: {added} new, {len(existing)} total in {args.output}")

    # Snapshot
    if new_notices:
        snap_dir = os.path.join(os.path.dirname(args.output) or ".", "snapshots")
        print(f"snapshotting {len(new_notices)} new notice(s)...")
        snap_ok, snap_fail, att_total = 0, 0, 0
        for notice in new_notices:
            result = snapshot_notice(notice, cookies, jar, snap_dir)
            if result.get("skipped"): snap_ok += 1
            elif result.get("error"):
                snap_fail += 1
                print(f"  FAIL {notice['url']}: {result['error']}")
            else:
                snap_ok += 1
                atts = result.get("attachments", 0)
                att_total += atts
                print(f"  OK {notice['title'][:30]}..." if not atts else f"  OK {notice['title'][:30]}... ({atts} att URLs)")
        print(f"snapshot: {snap_ok} ok, {snap_fail} fail, {att_total} attachment URLs recorded")


def fetch_section(info_url, cookies, jar, from_str):
    """Fetch all notices from one section, paginating as needed."""
    final_url, html = http_get(info_url, cookies, jar)
    hops = 0
    while html and "Newslist" not in html and hops < 3:
        js_url = extract_js_redirect(html)
        if not js_url: break
        final_url, html = http_get(js_url, cookies, jar)
        hops += 1
    if not html:
        raise RuntimeError(f"empty response while fetching section {info_url}")
    if "authserver/login" in html or "统一身份认证" in html or "帐号登录" in html:
        raise RuntimeError(f"cookies expired while fetching section {info_url}")
    if "Newslist" not in html:
        raise RuntimeError(f"unexpected info portal HTML while fetching section {info_url} (len={len(html)})")

    notices = extract_notices(html, final_url)
    pages = 1
    while notices and notices[-1].get("date", "") >= from_str:
        next_url = find_next_page_url(html, final_url)
        if not next_url: break
        next_final, next_html = http_get(next_url, cookies, jar)
        page_notices = extract_notices(next_html, next_final)
        if not page_notices: break
        notices.extend(page_notices)
        html = next_html
        final_url = next_final
        pages += 1
    return notices, pages

if __name__ == "__main__":
    main()
