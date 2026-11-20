"""Extraction-stage benchmark: Firecrawl /v2/scrape markdown vs Magnet trafilatura.

Input: Magnet's judged-eval run (eval/results_stage5.json) -> top-k URLs.
Politeness: robots.txt pre-check with Magnet's fetchlib; honest UA is sent to
Firecrawl in the request headers; at most one in-flight request per host and
>=1.2 s between requests to the same host. No proxies, no stealth.
"""
from __future__ import annotations

import argparse
import asyncio
import collections
import json
import os
import time
from urllib.parse import urlsplit

from curl_cffi import requests as cr

HERE = os.path.dirname(os.path.abspath(__file__))
WT = os.path.dirname(HERE)
sys_path = [WT]


def norm(u: str) -> str:
    p = urlsplit(u.strip())
    path = p.path or "/"
    if path != "/" and path.endswith("/"):
        path = path[:-1]
    host = p.netloc.lower()
    if host.startswith("www."):
        host = host[4:]
    return host + path


def pick_urls(results_path: str, limit: int) -> list[str]:
    with open(results_path, encoding="utf-8") as f:
        d = json.load(f)
    rows = d["cold"]
    seen, out = set(), []
    for rank in range(5):
        for r in rows:
            urls = r.get("top_urls") or []
            if rank < len(urls):
                u = urls[rank]
                if u not in seen:
                    seen.add(u)
                    out.append(u)
                    if len(out) >= limit:
                        return out
    return out


async def firecrawl_scrape(session, api: str, url: str, ua: str, timeout_ms=60000):
    body = {
        "url": url,
        "formats": ["markdown"],
        "headers": {"User-Agent": ua},
        "timeout": timeout_ms,
        "onlyMainContent": True,
    }
    t0 = time.perf_counter()
    try:
        r = await session.post(api.rstrip("/") + "/v2/scrape", json=body, timeout=timeout_ms / 1000 + 30)
        ms = (time.perf_counter() - t0) * 1000
        try:
            data = r.json()
        except Exception:
            return {"ok": False, "http": r.status_code, "error": (r.text or "")[:300], "ms": ms}
        if not data.get("success"):
            return {"ok": False, "http": r.status_code, "error": str(data.get("error"))[:300], "ms": ms}
        d = data.get("data") or {}
        md = d.get("markdown") or ""
        meta = d.get("metadata") or {}
        return {
            "ok": True,
            "ms": ms,
            "markdown": md,
            "statusCode": meta.get("statusCode"),
            "contentType": meta.get("contentType"),
            "source": meta.get("source"),
            "cacheState": meta.get("cacheState"),
            "warning": d.get("warning"),
            "title": meta.get("title"),
        }
    except Exception as exc:
        return {"ok": False, "http": None, "error": repr(exc)[:300], "ms": (time.perf_counter() - t0) * 1000}


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", required=True)
    ap.add_argument("--magnet-data", required=True)
    ap.add_argument("--limit", type=int, default=100)
    ap.add_argument("--api", default="http://localhost:3002")
    ap.add_argument("--out", default=os.path.join(HERE, "out", "extract.jsonl"))
    ap.add_argument("--concurrency", type=int, default=4)
    ap.add_argument("--ua", default="KrikoMagnet/0.2 (+https://kriko.cc/bot)")
    args = ap.parse_args()

    import sys
    sys.path.insert(0, WT)
    from fetchlib import Fetcher

    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    urls = pick_urls(args.results, args.limit)
    print(f"{len(urls)} urls", flush=True)

    fetcher = Fetcher(cache_path=os.path.join(args.magnet_data, "bench_fetch.sqlite"))
    hosts = collections.defaultdict(list)
    for u in urls:
        hosts[urlsplit(u).netloc.lower()].append(u)

    sem = asyncio.Semaphore(args.concurrency)
    session = cr.AsyncSession()
    lock = asyncio.Lock()
    done = {"n": 0}

    async def do_host(host, host_urls):
        async with sem:
            for url in host_urls:
                denied = False
                try:
                    allowed = await fetcher.allowed(url)
                    denied = allowed is False
                except Exception:
                    allowed = True
                rec = {"url": url, "host": host, "robots_denied": denied}
                if denied:
                    rec.update(firecrawl={"ok": False, "blocked": "robots"})
                else:
                    fc = await firecrawl_scrape(session, args.api, url, args.ua)
                    md = fc.pop("markdown", None)
                    if md is not None:
                        path = os.path.join(os.path.dirname(args.out), "fc", str(abs(hash(url))) + ".md")
                        os.makedirs(os.path.dirname(path), exist_ok=True)
                        with open(path, "w", encoding="utf-8") as f:
                            f.write(md)
                        fc["markdown_chars"] = len(md)
                        fc["path"] = os.path.basename(path)
                    rec["firecrawl"] = fc
                    await asyncio.sleep(1.2)
                async with lock:
                    done["n"] += 1
                    n = done["n"]
                print(f"[{n:03d}/{len(urls)}] {'DENY' if denied else ('ok ' if rec['firecrawl'].get('ok') else 'FAIL')} "
                      f"{rec['firecrawl'].get('ms', 0):7.0f}ms {url[:90]}", flush=True)
                with open(args.out, "a", encoding="utf-8") as f:
                    f.write(json.dumps(rec, ensure_ascii=False) + "\n")

    open(args.out, "w").close()
    await asyncio.gather(*(do_host(h, us) for h, us in hosts.items()))
    await session.close()
    await fetcher.close()


if __name__ == "__main__":
    asyncio.run(main())
