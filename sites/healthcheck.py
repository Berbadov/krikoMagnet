"""Adapter health check: one known query per site, links + extraction."""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys
import time
from urllib.parse import urlparse

from fetchlib import Fetcher, extract_text

from .adapters import run_site
from .discovery import BREAKER
from .registry import load_registry

HEALTH_QUERIES = {
    "gsmarena": "iphone 15",
    "rtings": "lg c1",
    "donanimhaber": "iphone 15",
    "cpsc": "anker",
    "ifixit": "iphone screen replacement",
    "vaillant": "ecotec fault code",
    "vaillanttr": "kombi arıza",
    "consumeraffairs": "samsung refrigerator",
    "whathifi": "lg oled",
    "macrumors": "iphone battery",
    "tomshardware": "rtx 4070",
    "caranddriver": "honda cr-v",
    "webtekno": "iphone 15",
    "chip": "iphone 15",
    "guvensizurun": "camasir",
    "boschhome": "bulaşık makinesi",
    "eca": "kombi arıza",
    "baymak": "kombi arıza",
    "donanimarsivi": "iphone inceleme",
    "shiftdelete": "iphone inceleme",
    "tamindir": "iphone",
}


async def check_site(site: dict, query: str, fetcher, test_extract: bool = True) -> dict:
    t0 = time.perf_counter()
    outcomes = await run_site(site, query, fetcher)
    links = [c for o in outcomes for c in o.candidates]
    top = links[0] if links else None
    extracted_chars = 0
    extract_status = ""
    if top is not None and test_extract:
        res = await fetcher.get(top.url)
        if res.robots_skipped:
            extract_status = "robots-denied"
        elif not res.ok:
            extract_status = f"HTTP {res.status}"
        else:
            text = extract_text(res.text)
            extracted_chars = len(text)
            extract_status = "ok" if extracted_chars >= 300 else "short"
    return {
        "site": site["id"],
        "domain": site["domain"],
        "method": outcomes[0].method if outcomes else "?",
        "status": outcomes[-1].status if outcomes else "no-method",
        "links": len(links),
        "top_url": top.url if top else "",
        "top_title": top.title[:90] if top else "",
        "extract": extract_status,
        "chars": extracted_chars,
        "fetched": sum(o.fetched for o in outcomes),
        "secs": round(time.perf_counter() - t0, 1),
    }


async def run(site_ids=None, test_extract=True, breaker=None):
    registry = load_registry()
    sites = [s for s in registry["sites"] if not site_ids or s["id"] in site_ids]
    fetcher = Fetcher(concurrency=8)
    try:
        results = await asyncio.gather(*[
            check_site(s, HEALTH_QUERIES.get(s["id"], "review"), fetcher, test_extract)
            for s in sites
        ])
    finally:
        await fetcher.close()
    breaker = breaker or BREAKER
    for row in results:
        state = breaker.state(row["site"])
        row["circuit_open"] = state["open"]
        row["failures"] = state["failures"]
    return results


def index_report(path: str) -> dict | None:
    if not path or not os.path.exists(path):
        return None
    try:
        from .index import SiteIndex
        index = SiteIndex(path)
        try:
            return index.stats().as_dict()
        finally:
            index.close()
    except Exception as exc:
        return {"error": repr(exc)[:160]}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--site", action="append", help="check only this site id (repeatable)")
    ap.add_argument("--no-extract", action="store_true", help="skip extraction test")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--index", default=os.path.join(os.path.dirname(__file__), "..", "data", "site_index.sqlite"))
    args = ap.parse_args()
    results = asyncio.run(run(set(args.site) if args.site else None, not args.no_extract))
    index = index_report(args.index)
    ok = 0
    for r in results:
        r["healthy"] = bool(r["links"]) and (args.no_extract or r["extract"] == "ok")
        if r["healthy"]:
            ok += 1
        else:
            BREAKER.failure(r["site"])
        r["circuit_open"] = BREAKER.is_open(r["site"])
    unhealthy = [r["site"] for r in results if not r["healthy"]]
    report = {
        "healthy": ok == len(results),
        "sites_ok": ok,
        "sites_total": len(results),
        "unhealthy": unhealthy,
        "circuit_open": [s for s, st in BREAKER.snapshot().items() if st["open"]],
        "index": index,
        "sites": results,
    }
    if args.json:
        print(json.dumps(report, ensure_ascii=False, indent=1))
        return 0 if report["healthy"] else 1
    print(f"{'site':<16}{'method':<10}{'status':<12}{'links':<7}{'extract':<10}{'chars':<8}{'fetched':<8}{'secs':<6} top")
    print("-" * 130)
    for r in results:
        print(f"{r['site']:<16}{r['method']:<10}{r['status']:<12}{r['links']:<7}"
              f"{r['extract']:<10}{r['chars']:<8}{r['fetched']:<8}{r['secs']:<6} {r['top_url'][:60]}")
    print("-" * 130)
    print(f"healthy (links + extraction): {ok}/{len(results)}")
    if index:
        age = index.get("age_seconds")
        print(f"local index: {index.get('pages')} pages / {index.get('sites')} sites, "
              f"age {age if age is not None else 'n/a'}s")
    open_sites = report["circuit_open"]
    if open_sites:
        print(f"circuit open: {', '.join(open_sites)}")
    if unhealthy:
        print(f"unhealthy: {', '.join(unhealthy)}")
    return 0 if report["healthy"] else 1


if __name__ == "__main__":
    sys.exit(main())
