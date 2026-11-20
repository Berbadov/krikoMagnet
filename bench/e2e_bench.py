"""End-to-end benchmark: Firecrawl /v2/search (local SearXNG) + /v2/scrape top-5.

Judged with Magnet's eval/queries.jsonl and the same URL normalisation and
nDCG@5 definition as eval/run_eval.py.

Politeness: robots.txt is pre-checked with Magnet's fetchlib for every
discovered URL (Firecrawl's scrape does not check robots by default); denied
URLs are never sent to Firecrawl. Queries run one at a time, page fetches are
serialised per host with >=1.2 s spacing, and the honest UA is passed in the
scrape headers.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import math
import os
import statistics
import time
from urllib.parse import unquote, urlsplit, urlunsplit

from curl_cffi import requests as cr

HERE = os.path.dirname(os.path.abspath(__file__))
WT = os.path.dirname(HERE)


def normalize_url(url: str) -> str:
    parts = urlsplit(url.strip())
    path = unquote(parts.path or "/")
    if path != "/" and path.endswith("/"):
        path = path[:-1]
    scheme = "https" if parts.scheme in ("http", "https") else parts.scheme
    host = parts.netloc.lower()
    if host.startswith("www."):
        host = host[4:]
    return urlunsplit((scheme, host, path, parts.query, ""))


def dcg(flags, k):
    return sum(rel / math.log2(i + 2) for i, rel in enumerate(flags[:k]))


def ndcg(flags, n_relevant, k):
    ideal = sum(1.0 / math.log2(i + 2) for i in range(min(k, n_relevant)))
    return dcg(flags, k) / ideal if ideal else 0.0


def load_queries(path):
    out = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                out.append(json.loads(line))
    return out


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--queries", required=True)
    ap.add_argument("--api", default="http://localhost:3002")
    ap.add_argument("--k", type=int, default=5)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--warm-repeat", type=int, default=10)
    ap.add_argument("--out", default=os.path.join(HERE, "out", "e2e.jsonl"))
    ap.add_argument("--ua", default="KrikoMagnet/0.2 (+https://kriko.cc/bot)")
    args = ap.parse_args()

    import sys
    sys.path.insert(0, WT)
    from fetchlib import Fetcher

    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    queries = load_queries(args.queries)
    if args.limit:
        queries = queries[: args.limit]
    fetcher = Fetcher(cache_path=os.path.join(WT, "data", "bench_fetch.sqlite"))
    session = cr.AsyncSession()
    open(args.out, "w").close()

    async def search(q):
        t0 = time.perf_counter()
        r = await session.post(
            args.api.rstrip("/") + "/v2/search",
            json={"query": q, "limit": args.k, "sources": [{"type": "web"}], "timeout": 30000},
            timeout=90,
        )
        ms = (time.perf_counter() - t0) * 1000
        try:
            data = r.json()
        except Exception:
            return [], ms, {"http": r.status_code, "error": (r.text or "")[:200]}
        if not data.get("success"):
            return [], ms, {"http": r.status_code, "error": str(data.get("error"))[:200]}
        d = data.get("data") or {}
        web = d.get("web") or []
        return [w.get("url") for w in web if w.get("url")], ms, {"http": r.status_code}

    async def scrape(url):
        t0 = time.perf_counter()
        try:
            r = await session.post(
                args.api.rstrip("/") + "/v2/scrape",
                json={
                    "url": url,
                    "formats": ["markdown"],
                    "headers": {"User-Agent": args.ua},
                    "timeout": 60000,
                    "onlyMainContent": True,
                },
                timeout=90,
            )
            ms = (time.perf_counter() - t0) * 1000
            data = r.json()
            if not data.get("success"):
                return {"ok": False, "http": r.status_code, "error": str(data.get("error"))[:120], "ms": ms}
            d = data.get("data") or {}
            return {"ok": True, "http": r.status_code, "chars": len(d.get("markdown") or ""),
                    "status": (d.get("metadata") or {}).get("statusCode"), "ms": ms}
        except Exception as exc:
            return {"ok": False, "error": repr(exc)[:120], "ms": (time.perf_counter() - t0) * 1000}

    async def run_query(q, phase):
        qid, text = q["id"], q["query"]
        urls, search_ms, sinfo = await search(text)
        kept, denied = [], []
        for u in urls:
            try:
                allowed = await fetcher.allowed(u)
            except Exception:
                allowed = True
            (denied if allowed is False else kept).append(u)
        results = []
        host_last = {}
        t0 = time.perf_counter()
        for u in kept[: args.k]:
            host = urlsplit(u).netloc.lower()
            wait = host_last.get(host, 0) + 1.2 - time.monotonic()
            if wait > 0:
                await asyncio.sleep(wait)
            host_last[host] = time.monotonic()
            sc = await scrape(u)
            sc["url"] = u
            results.append(sc)
        scrape_ms = (time.perf_counter() - t0) * 1000
        relevant = {normalize_url(u) for u in q.get("relevant", [])}
        flags = [1 if normalize_url(r["url"]) in relevant else 0 for r in results]
        n_rel = len(relevant)
        rec = {
            "id": qid,
            "phase": phase,
            "query": text,
            "language": q.get("language", ""),
            "judged": q.get("judged", True),
            "search_urls": urls,
            "search_ms": search_ms,
            "denied": denied,
            "results": results,
            "top_urls": [r["url"] for r in results],
            "flags": flags,
            "hit": 1 if any(flags) else 0,
            "ndcg": ndcg(flags, n_rel, args.k) if n_rel else 0.0,
            "precision": sum(flags) / args.k,
            "scrape_ms": scrape_ms,
            "total_ms": search_ms + scrape_ms,
            "requests": 1 + len(results) + len(denied),
            "search_info": sinfo,
        }
        with open(args.out, "a", encoding="utf-8") as f:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
        print(f"[{phase} {qid}] {'HIT ' if rec['hit'] else 'miss'} ndcg={rec['ndcg']:.2f} "
              f"urls={len(urls)} denied={len(denied)} {rec['total_ms']:6.0f}ms {text[:44]}", flush=True)
        return rec

    cold = []
    for q in queries:
        cold.append(await run_query(q, "cold"))
    warm = []
    for q in queries[: args.warm_repeat]:
        warm.append(await run_query(q, "warm"))

    await session.close()
    await fetcher.close()

    def summarize(rows, label):
        judged = [r for r in rows if r["judged"]]
        lat = sorted(r["total_ms"] for r in judged)
        by_lang = {}
        for lang in ("en", "tr"):
            sub = [r for r in judged if r["language"] == lang]
            if sub:
                by_lang[lang] = {
                    "n": len(sub),
                    "hit@5": sum(r["hit"] for r in sub) / len(sub),
                    "ndcg@5": statistics.mean(r["ndcg"] for r in sub),
                }
        return {
            "label": label,
            "n": len(judged),
            "hit@5": sum(r["hit"] for r in judged) / len(judged) if judged else 0,
            "ndcg@5": statistics.mean(r["ndcg"] for r in judged) if judged else 0,
            "p@5": statistics.mean(r["precision"] for r in judged) if judged else 0,
            "latency_p50_ms": statistics.median(lat) if lat else 0,
            "latency_p95_ms": lat[min(len(lat) - 1, int(len(lat) * 0.95))] if lat else 0,
            "search_p50_ms": statistics.median(sorted(r["search_ms"] for r in judged)) if judged else 0,
            "requests_mean": statistics.mean(r["requests"] for r in judged) if judged else 0,
            "robots_denied_total": sum(len(r["denied"]) for r in judged),
            "scrape_fail_total": sum(1 for r in judged for x in r["results"] if not x["ok"]),
            "by_lang": by_lang,
        }

    summary = {"cold": summarize(cold, "cold"), "warm": summarize(warm, "warm")}
    with open(os.path.join(os.path.dirname(args.out), "e2e_summary.json"), "w", encoding="utf-8") as f:
        json.dump(summary, f, indent=1)
    print(json.dumps(summary, indent=1))


if __name__ == "__main__":
    asyncio.run(main())
