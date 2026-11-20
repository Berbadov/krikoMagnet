"""Adapter health check: one known query per site, links + extraction."""
from __future__ import annotations

import argparse
import asyncio
import json
import sys
import time
from urllib.parse import urlparse

from fetchlib import Fetcher, extract_text

from .adapters import run_site
from .registry import load_registry

HEALTH_QUERIES = {
    "gsmarena": "iphone 15",
    "rtings": "lg c1",
    "donanimhaber": "iphone 15",
    "cpsc": "anker",
    "ifixit": "iphone screen replacement",
    "vaillant": "ecotec fault code",
    "consumeraffairs": "samsung refrigerator",
    "whathifi": "lg oled",
    "macrumors": "iphone battery",
    "tomshardware": "rtx 4070",
    "caranddriver": "honda cr-v",
    "webtekno": "iphone 15",
    "chip": "iphone 15",
    "guvensizurun": "camasir",
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


async def run(site_ids=None, test_extract=True):
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
    return results


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--site", action="append", help="check only this site id (repeatable)")
    ap.add_argument("--no-extract", action="store_true", help="skip extraction test")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()
    results = asyncio.run(run(set(args.site) if args.site else None, not args.no_extract))
    if args.json:
        print(json.dumps(results, ensure_ascii=False, indent=1))
        return
    print(f"{'site':<16}{'method':<10}{'status':<12}{'links':<7}{'extract':<10}{'chars':<8}{'fetched':<8}{'secs':<6} top")
    print("-" * 130)
    ok = 0
    for r in results:
        if r["links"] and r["extract"] == "ok":
            ok += 1
        print(f"{r['site']:<16}{r['method']:<10}{r['status']:<12}{r['links']:<7}"
              f"{r['extract']:<10}{r['chars']:<8}{r['fetched']:<8}{r['secs']:<6} {r['top_url'][:60]}")
    print("-" * 130)
    print(f"healthy (links + extraction): {ok}/{len(results)}")
    return 0 if ok == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
