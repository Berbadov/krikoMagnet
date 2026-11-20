"""Magnet page-fetch -> markdown latency on the same URL set (one per host).

Politeness: fetchlib robots.txt + honest UA + >=1.2 s per host. Fresh fetch
(separate cache), so this excludes sitemap/discovery work.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import statistics
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
WT = os.path.dirname(HERE)
sys.path.insert(0, WT)

from fetchlib import Fetcher  # noqa: E402
from fetchlib.extract import extract_text  # noqa: E402


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--fclog", default=os.path.join(HERE, "out", "extract.jsonl"))
    ap.add_argument("--limit", type=int, default=20)
    ap.add_argument("--out", default=os.path.join(HERE, "out", "magnet_fetch.json"))
    args = ap.parse_args()

    rows = [json.loads(l) for l in open(args.fclog, encoding="utf-8") if l.strip()]
    seen, urls = set(), []
    for r in rows:
        if r["host"] in seen:
            continue
        seen.add(r["host"])
        urls.append(r["url"])
        if len(urls) >= args.limit:
            break

    f = Fetcher(cache_path=os.path.join(WT, "data", "bench_fetch.sqlite"), cache_ttl=0)
    out = []
    for u in urls:
        t0 = time.perf_counter()
        res = await f.get(u, use_cache=False)
        fetch_ms = (time.perf_counter() - t0) * 1000
        t1 = time.perf_counter()
        text = extract_text(res.text or "")
        ext_ms = (time.perf_counter() - t1) * 1000
        out.append({"url": u, "status": res.status, "robots_denied": res.robots_skipped,
                    "fetch_ms": fetch_ms, "extract_ms": ext_ms,
                    "total_ms": fetch_ms + ext_ms, "chars": len(text)})
        print(f"{res.status} {fetch_ms:6.0f}+{ext_ms:5.0f}ms chars={len(text):6d} {u[:80]}", flush=True)
        await asyncio.sleep(1.2)
    await f.close()
    tot = sorted(r["total_ms"] for r in out if r["status"] == 200)
    json.dump(out, open(args.out, "w", encoding="utf-8"), indent=1)
    if tot:
        print("p50", round(statistics.median(tot)), "p95", round(tot[min(len(tot) - 1, int(len(tot) * 0.95))]))


if __name__ == "__main__":
    asyncio.run(main())
