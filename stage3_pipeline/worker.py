"""Async fetch worker: shared polite fetch layer + trafilatura extraction."""
import asyncio
import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))

from fetchlib import Fetcher, extract_text


async def fetch_one(fetcher, url):
    t0 = time.perf_counter()
    res = await fetcher.get(url)
    fetch_ms = (time.perf_counter() - t0) * 1000
    if res.status == "ERR":
        return {"url": url, "status": "ERR", "html_bytes": 0,
                "markdown": "", "fetch_ms": fetch_ms, "parse_ms": 0,
                "error": res.error, "from_cache": False, "robots_allowed": res.robots_allowed}
    if res.robots_skipped:
        return {"url": url, "status": "robots", "html_bytes": 0,
                "markdown": "", "fetch_ms": fetch_ms, "parse_ms": 0,
                "error": "robots.txt disallows this path", "from_cache": False,
                "robots_allowed": False}

    t1 = time.perf_counter()
    md = extract_text(res.text) if res.ok else ""
    parse_ms = (time.perf_counter() - t1) * 1000
    return {
        "url": url, "status": res.status, "html_bytes": res.size,
        "markdown": md, "fetch_ms": fetch_ms, "parse_ms": parse_ms,
        "error": None, "from_cache": res.from_cache,
        "robots_allowed": res.robots_allowed,
    }


async def fetch_many(urls, concurrency=4, stats=None, fetcher=None):
    owns_fetcher = fetcher is None
    if fetcher is None:
        fetcher = Fetcher(concurrency=concurrency)
    sem = asyncio.Semaphore(concurrency)
    results = {}

    async def worker(url):
        async with sem:
            fr = await fetch_one(fetcher, url)
            results[url] = fr
            if stats is not None:
                if not fr["from_cache"]:
                    stats["bytes"] += fr["html_bytes"]
                    stats["fetches"] += 1
                if fr["status"] == "robots":
                    stats["robots_skips"] = stats.get("robots_skips", 0) + 1

    try:
        await asyncio.gather(*[worker(u) for u in urls])
    finally:
        if owns_fetcher:
            await fetcher.close()
    return results
