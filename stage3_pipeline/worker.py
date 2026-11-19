"""Async fetch worker: curl_cffi (impersonate=chrome) + trafilatura extraction
with selectolax fallback, mirroring the Stage 2 pipeline."""
import asyncio
import time

from curl_cffi import requests as cr

SOFT_MIN_CHARS = 300


def extract_text(html: str) -> str:
    text = None
    try:
        import trafilatura
        text = trafilatura.extract(html, include_comments=False, include_tables=True, favor_recall=True)
    except Exception:
        text = None
    if text and len(text) >= SOFT_MIN_CHARS:
        return text
    try:
        from selectolax.parser import HTMLParser
        tree = HTMLParser(html)
        body = tree.body
        text = body.text(separator=" ") if body is not None else ""
        text = " ".join(text.split())
    except Exception:
        text = text or ""
    return text


async def fetch_one(session, url):
    t0 = time.perf_counter()
    try:
        r = await session.get(url, timeout=25, allow_redirects=True)
        html = r.text
        status = r.status_code
        html_bytes = len(r.content)
    except Exception as e:
        return {"url": url, "status": "ERR", "html_bytes": 0,
                "markdown": "", "fetch_ms": (time.perf_counter() - t0) * 1000,
                "error": repr(e)[:80]}
    t1 = time.perf_counter()
    md = extract_text(html)
    parse_ms = (time.perf_counter() - t1) * 1000
    return {
        "url": url, "status": status, "html_bytes": html_bytes,
        "markdown": md, "fetch_ms": (time.perf_counter() - t0) * 1000,
        "parse_ms": parse_ms, "error": None,
    }


async def fetch_many(urls, concurrency=4, stats=None):
    sem = asyncio.Semaphore(concurrency)
    results = {}

    async def worker(url):
        async with sem:
            async with cr.AsyncSession(impersonate="chrome") as s:
                fr = await fetch_one(s, url)
                results[url] = fr
                if stats is not None:
                    stats["bytes"] += fr["html_bytes"]
                    stats["fetches"] += 1

    await asyncio.gather(*[worker(u) for u in urls])
    return results
