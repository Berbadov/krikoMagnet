"""Stage 2: direct website fetching & block-rate telemetry.

Fetches 40 real URLs through the shared polite fetch layer (robots.txt,
honest user agent, per-host spacing, global concurrency cap, 429 backoff),
extracts clean main-body text with trafilatura (selectolax fallback), and
prints a per-domain telemetry table.
"""
import asyncio
import json
import os
import sys
import time
from urllib.parse import urlparse

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))

from fetchlib import Fetcher, classify_fetch, extract_text

CONCURRENCY = 8


async def fetch_one(fetcher, entry):
    url = entry["url"]
    t0 = time.perf_counter()
    res = await fetcher.get(url)
    fetch_ms = (time.perf_counter() - t0) * 1000
    if res.robots_skipped:
        return {**entry, "status": "robots", "fetch_ms": fetch_ms, "parse_ms": 0,
                "clean_chars": 0, "marker": False, "verdict": "robots-denied",
                "from_cache": res.from_cache}

    t1 = time.perf_counter()
    text = extract_text(res.text) if res.ok else ""
    parse_ms = (time.perf_counter() - t1) * 1000
    status = res.status if res.status != "ERR" else "ERR"
    if status == "ERR":
        verdict = "network-error"
    else:
        verdict = classify_fetch(status, len(text), text)
    return {
        **entry, "status": status, "fetch_ms": fetch_ms, "parse_ms": parse_ms,
        "clean_chars": len(text), "marker": verdict == "soft-block",
        "verdict": verdict, "from_cache": res.from_cache,
        "robots_allowed": res.robots_allowed,
    }


async def main():
    with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "urls.json"), encoding="utf-8") as f:
        entries = json.load(f)
    fetcher = Fetcher(concurrency=CONCURRENCY)
    results = []
    sem = asyncio.Semaphore(CONCURRENCY)

    async def worker(e):
        async with sem:
            results.append(await fetch_one(fetcher, e))

    try:
        await asyncio.gather(*[worker(e) for e in entries])
    finally:
        await fetcher.close()

    results.sort(key=lambda x: (x["verdict"], x["url"]))
    print("=" * 118)
    print(f"{'domain':<28}{'cat':<20}{'status':<7}{'clean_chars':<12}{'fetch_ms':<9}{'parse_ms':<9}{'verdict'}")
    print("-" * 118)
    for r in results:
        dom = urlparse(r["url"]).netloc
        print(f"{dom:<28}{r['category']:<20}{str(r['status']):<7}{r['clean_chars']:<12}{r['fetch_ms']:<9.0f}{r['parse_ms']:<9.1f}{r['verdict']}")
    print("=" * 118)

    n = len(results)
    succ = [r for r in results if r["verdict"] == "success"]
    hard = [r for r in results if r["verdict"] == "hard-block"]
    soft = [r for r in results if r["verdict"] == "soft-block"]
    http = [r for r in results if r["verdict"] == "http-error"]
    neterr = [r for r in results if r["verdict"] == "network-error"]
    robots = [r for r in results if r["verdict"] == "robots-denied"]
    fetchable = n - len(robots)
    success_rate = len(succ) / fetchable if fetchable else 0.0
    block_rate = (len(hard) + len(soft)) / fetchable if fetchable else 0.0
    print(f"\nTOTAL targets: {n}")
    print(f"clean success: {len(succ)}  ({success_rate*100:.1f}% of fetchable)")
    print(f"hard blocks:   {len(hard)}   soft blocks: {len(soft)}   total blocks: {len(hard)+len(soft)} ({block_rate*100:.1f}% of fetchable)")
    print(f"http errors:   {len(http)}   network errors: {len(neterr)}   robots skips: {len(robots)}")

    if hard or soft or robots:
        print("\nBLOCKED / SKIPPED:")
        for r in hard:
            print(f"  - {urlparse(r['url']).netloc:<28} status={r['status']} (hard)")
        for r in soft:
            print(f"  - {urlparse(r['url']).netloc:<28} chars={r['clean_chars']} (soft)")
        for r in robots:
            print(f"  - {urlparse(r['url']).netloc:<28} robots.txt disallows this path (skipped, not fetched)")

    print("\n" + "=" * 60)
    print(f"fetchable targets: {fetchable} (robots skips excluded)")
    print(f"GATE 2 success_rate={success_rate*100:.1f}% (need >=75%), block_rate={block_rate*100:.1f}% (need <=25%)")
    print("GATE 2: PASS" if fetchable and success_rate >= 0.75 and block_rate <= 0.25 else "GATE 2: FAIL")


if __name__ == "__main__":
    asyncio.run(main())
