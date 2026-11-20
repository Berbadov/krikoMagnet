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


async def fetch_many(urls, concurrency=4, stats=None, fetcher=None, budget_ms=0, min_one=False):
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

    tasks = {asyncio.create_task(worker(u)): u for u in urls}
    try:
        if budget_ms and budget_ms > 0:
            done, pending = await asyncio.wait(tasks, timeout=budget_ms / 1000.0)
            if min_one and not results and pending:
                await asyncio.wait(pending, timeout=1.5,
                                   return_when=asyncio.FIRST_COMPLETED)
                pending = {t for t in pending if not t.done()}
            for task in pending:
                task.cancel()
            if pending:
                await asyncio.gather(*pending, return_exceptions=True)
        else:
            await asyncio.gather(*tasks)
    finally:
        if owns_fetcher:
            await fetcher.close()
    return results
