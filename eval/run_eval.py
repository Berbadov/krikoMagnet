"""Honest evaluation harness for Kriko Magnet discovery.

Runs the registry-driven Stage 3 pipeline on eval/queries.jsonl, judges the
top-k URLs against the hand-checked relevance sets, and reports hit rate,
nDCG@5, sources, latency (cold/warm), network fetches and block rate.

No search-engine provider is involved.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import math
import os
import statistics
import sys
import time
from urllib.parse import unquote, urlsplit, urlunsplit

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "stage3_pipeline"))
sys.path.insert(0, ROOT)

from fetchlib import Fetcher  # noqa: E402
from cache import Cache  # noqa: E402
from pipeline import Searcher  # noqa: E402

QUERIES = os.path.join(HERE, "queries.jsonl")
DEFAULT_CACHE = os.path.join(ROOT, "data", "eval_cache.sqlite")
DEFAULT_FETCH_CACHE = os.path.join(ROOT, "data", "eval_fetch.sqlite")
BLOCK_STATUSES = {401, 403, 429, "robots"}


def safe(text: str) -> str:
    return text.encode("ascii", "ignore").decode("ascii")


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


def load_queries(path=QUERIES):
    out = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                out.append(json.loads(line))
    return out


def dcg(flags, k):
    return sum(rel / math.log2(i + 2) for i, rel in enumerate(flags[:k]))


def ndcg(flags, n_relevant, k):
    ideal = sum(1.0 / math.log2(i + 2) for i in range(min(k, n_relevant)))
    return dcg(flags, k) / ideal if ideal else 0.0


async def run_once(searcher: Searcher, queries, k: int):
    rows = []
    for i, q in enumerate(queries, 1):
        t0 = time.perf_counter()
        payload = await searcher.search(q["query"], k=k)
        elapsed = (time.perf_counter() - t0) * 1000
        results = payload.get("results", [])
        judged = q.get("judged", True)
        relevant = {normalize_url(u) for u in q.get("relevant", [])}
        flags = [1 if normalize_url(r["url"]) in relevant else 0 for r in results]
        n_rel = len(relevant)
        plan = payload.get("plan", {})
        stats = payload.get("fetch_stats", {})
        statuses = [r.get("status") for r in results]
        blocks = sum(1 for s in statuses if s in BLOCK_STATUSES)
        short = sum(1 for r in results if len(r.get("clean_markdown") or "") < 300)
        rows.append({
            "id": q["id"],
            "query": q["query"],
            "language": q.get("language", ""),
            "judged": judged,
            "topics": plan.get("topics", q.get("topics", [])),
            "sites": plan.get("sites", []),
            "sources_with_results": plan.get("sources_with_results", 0),
            "result_domains": len({r.get("domain") for r in results if r.get("domain")}),
            "k": k,
            "results": len(results),
            "relevant": sorted(relevant),
            "top_urls": [r["url"] for r in results],
            "flags": flags,
            "hit": 1 if any(flags) else 0,
            "precision": sum(flags) / k if k else 0.0,
            "ndcg": ndcg(flags, n_rel, k) if n_rel else 0.0,
            "elapsed_ms": elapsed,
            "payload_ms": payload.get("elapsed_ms", elapsed),
            "fetches": stats.get("total_fetches", 0),
            "bytes": stats.get("total_bytes", 0),
            "robots_skips": stats.get("robots_skips", 0),
            "blocks": blocks,
            "short_extractions": short,
            "statuses": statuses,
            "notes": q.get("notes", ""),
        })
        mark = "HIT " if rows[-1]["hit"] else "miss"
        print(f"[{i:02d}/{len(queries)}] {mark} nDCG={rows[-1]['ndcg']:.3f} "
              f"{elapsed:7.0f}ms fetch={rows[-1]['fetches']:3d} "
              f"{safe(q['query'])[:56]}", flush=True)
    return rows


def summarize(rows, label):
    judged = [r for r in rows if r["judged"]]
    if not judged:
        return {"label": label, "n": 0}
    lat = sorted(r["elapsed_ms"] for r in judged)
    fetches = [r["fetches"] for r in judged]
    blocks = sum(r["blocks"] for r in judged)
    results = sum(r["results"] for r in judged)
    return {
        "label": label,
        "n": len(judged),
        "hit@5": sum(r["hit"] for r in judged) / len(judged),
        "ndcg@5": statistics.mean(r["ndcg"] for r in judged),
        "p@5": statistics.mean(r["precision"] for r in judged),
        "sources_mean": statistics.mean(r["sources_with_results"] for r in judged),
        "domains_mean": statistics.mean(r["result_domains"] for r in judged),
        "latency_p50_ms": statistics.median(lat),
        "latency_p95_ms": lat[min(len(lat) - 1, int(len(lat) * 0.95))],
        "fetches_mean": statistics.mean(fetches),
        "fetches_median": statistics.median(fetches),
        "fetches_total": sum(fetches),
        "robots_skips": sum(r["robots_skips"] for r in judged),
        "block_rate": blocks / results if results else 0.0,
        "short_rate": sum(r["short_extractions"] for r in judged) / results if results else 0.0,
    }


def print_report(cold, warm, warm_fetch, rows):
    print("\n" + "=" * 100)
    print("Kriko Magnet evaluation — registry discovery (no search engine)")
    print("=" * 100)
    print(f"judged queries: {cold['n']}   top-k: 5")
    print(f"queries with >=1 relevant result in top-5: {cold['hit@5']*100:.1f}% "
          f"({sum(r['hit'] for r in rows if r['judged'])}/{cold['n']})")
    print(f"nDCG@5 mean: {cold['ndcg@5']:.3f}   P@5 mean: {cold['p@5']:.3f}")
    print(f"sources returning candidates / query (mean): {cold['sources_mean']:.2f}")
    print(f"result domains / query (mean): {cold['domains_mean']:.2f}")
    print(f"cold latency p50/p95: {cold['latency_p50_ms']:.0f} / {cold['latency_p95_ms']:.0f} ms")
    print(f"warm-fetch, cold-query p50/p95: {warm_fetch['latency_p50_ms']:.0f} / {warm_fetch['latency_p95_ms']:.0f} ms")
    print(f"warm query-cache p50/p95: {warm['latency_p50_ms']:.0f} / {warm['latency_p95_ms']:.0f} ms")
    print(f"network fetches/query cold: mean {cold['fetches_mean']:.1f}, median {cold['fetches_median']:.0f}, total {cold['fetches_total']}")
    print(f"robots.txt skips (cold): {cold['robots_skips']}")
    print(f"block rate (hard/soft): {cold['block_rate']*100:.1f}%   short extractions: {cold['short_rate']*100:.1f}%")

    print("\n" + "-" * 100)
    print(f"{'id':<5}{'lang':<4}{'hit':<4}{'nDCG':<6}{'src':<4}{'ms':<7}{'fetch':<6}{'topics':<26}query")
    print("-" * 100)
    for r in rows:
        if not r["judged"]:
            continue
        print(f"{r['id']:<5}{r['language']:<4}{r['hit']:<4}{r['ndcg']:<6.2f}"
              f"{r['sources_with_results']:<4}{r['elapsed_ms']:<7.0f}{r['fetches']:<6}"
              f"{','.join(r['topics'])[:25]:<26}{safe(r['query'])[:42]}")
    print("-" * 100)

    unjudged = [r for r in rows if not r["judged"]]
    if unjudged:
        print("\nUnjudged coverage gaps (reported, not scored):")
        for r in unjudged:
            print(f"  {r['id']} {safe(r['query'])} — {safe(r['notes'])}")

    misses = [r for r in rows if r["judged"] and not r["hit"]]
    if misses:
        print("\nMisses (no relevant URL in top-5):")
        for r in misses:
            print(f"  {r['id']} {safe(r['query'])} | judged={len(r['relevant'])} | got:")
            for u in r["top_urls"]:
                print(f"      {u}")


def markdown_report(cold, warm, warm_fetch, rows):
    lines = []
    lines.append(f"| Metric | Value |")
    lines.append(f"|---|---|")
    lines.append(f"| Judged queries | {cold['n']} |")
    lines.append(f"| Queries with >=1 relevant in top-5 | {cold['hit@5']*100:.1f}% |")
    lines.append(f"| nDCG@5 (mean) | {cold['ndcg@5']:.3f} |")
    lines.append(f"| Precision@5 (mean) | {cold['p@5']:.3f} |")
    lines.append(f"| Sources with candidates / query | {cold['sources_mean']:.2f} |")
    lines.append(f"| Result domains / query | {cold['domains_mean']:.2f} |")
    lines.append(f"| Cold latency p50 / p95 | {cold['latency_p50_ms']:.0f} / {cold['latency_p95_ms']:.0f} ms |")
    lines.append(f"| Warm-fetch, cold-query p50 / p95 | {warm_fetch['latency_p50_ms']:.0f} / {warm_fetch['latency_p95_ms']:.0f} ms |")
    lines.append(f"| Warm query-cache p50 / p95 | {warm['latency_p50_ms']:.0f} / {warm['latency_p95_ms']:.0f} ms |")
    lines.append(f"| Network fetches / query (cold) | {cold['fetches_mean']:.1f} mean, {cold['fetches_median']:.0f} median |")
    lines.append(f"| robots.txt skips (cold) | {cold['robots_skips']} |")
    lines.append(f"| Block rate | {cold['block_rate']*100:.1f}% |")
    return "\n".join(lines)


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--k", type=int, default=5)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--fresh", action="store_true", help="delete eval caches before the cold run")
    ap.add_argument("--curated", action="store_true", help="use the old hand-curated Stage-3 fallback")
    ap.add_argument("--cache", default=DEFAULT_CACHE)
    ap.add_argument("--fetch-cache", default=DEFAULT_FETCH_CACHE)
    ap.add_argument("--json", default="")
    ap.add_argument("--markdown", default="")
    args = ap.parse_args()

    queries = load_queries()
    if args.limit:
        queries = queries[:args.limit]
    if args.fresh:
        for path in (args.cache, args.fetch_cache):
            for suffix in ("", "-wal", "-shm"):
                if os.path.exists(path + suffix):
                    os.unlink(path + suffix)

    fetcher = Fetcher(cache_path=args.fetch_cache)
    cache = Cache(args.cache)
    searcher = Searcher(cache=cache, top_k=args.k, discovery="curated" if args.curated else "registry",
                        fetcher=fetcher)

    print("=" * 100)
    print(f"COLD RUN — {len(queries)} queries, k={args.k}, discovery={searcher.discovery}")
    print("=" * 100)
    cold_rows = await run_once(searcher, queries, args.k)
    cold = summarize(cold_rows, "cold")

    print("\n" + "=" * 100)
    print("WARM-FETCH, COLD-QUERY — sitemaps/pages cached, query cache cleared")
    print("=" * 100)
    searcher.cache.conn.execute("DELETE FROM qcache")
    searcher.cache.conn.commit()
    warm_fetch_rows = await run_once(searcher, queries, args.k)
    warm_fetch = summarize(warm_fetch_rows, "warm_fetch")

    print("\n" + "=" * 100)
    print("WARM QUERY CACHE — exact repeats")
    print("=" * 100)
    warm_rows = await run_once(searcher, queries, args.k)
    warm = summarize(warm_rows, "warm")

    await searcher.close()
    await fetcher.close()

    print_report(cold, warm, warm_fetch, cold_rows)
    md = markdown_report(cold, warm, warm_fetch, cold_rows)
    print("\n" + md)

    if args.json:
        with open(args.json, "w", encoding="utf-8") as f:
            json.dump({"cold": cold_rows, "warm_fetch": warm_fetch_rows, "warm": warm_rows,
                       "summary": {"cold": cold, "warm_fetch": warm_fetch, "warm": warm}},
                      f, ensure_ascii=False, indent=1)
        print(f"\nwrote {args.json}")
    if args.markdown:
        with open(args.markdown, "w", encoding="utf-8") as f:
            f.write("# Kriko Magnet evaluation\n\n" + md + "\n")
        print(f"wrote {args.markdown}")


if __name__ == "__main__":
    asyncio.run(main())
