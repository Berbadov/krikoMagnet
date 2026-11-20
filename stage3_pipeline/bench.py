"""Stage 3 end-to-end benchmark: cold cache vs warm cache.

Default mode uses the site registry (query -> adapters -> Rust ranker ->
fetch/extract). ``--curated`` keeps the old hand-curated candidate path for
comparison with the original Stage 3 numbers.
"""
import argparse
import asyncio
import json
import os
import statistics
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.join(HERE, "..")
sys.path.insert(0, ROOT)

from cache import Cache
from pipeline import Searcher

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.join(HERE, "..")
KRIKO_QUERIES = os.path.join(HERE, "queries_kriko.json")
CURATED_QUERIES = os.path.join(HERE, "queries_pipeline.json")
BENCH_CACHE = os.path.join(ROOT, "data", "bench_cache.sqlite")
BENCH_FETCH = os.path.join(ROOT, "data", "bench_fetch.sqlite")


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--curated", action="store_true", help="use the old 26-URL candidate list")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--fresh", action="store_true", help="delete benchmark caches first")
    ap.add_argument("--k", type=int, default=3)
    ap.add_argument("--index", default=os.path.join(ROOT, "data", "site_index.sqlite"))
    ap.add_argument("--no-index", action="store_true")
    ap.add_argument("--page-budget", type=int, default=0)
    args = ap.parse_args()

    path = CURATED_QUERIES if args.curated else KRIKO_QUERIES
    with open(path, encoding="utf-8") as f:
        queries = json.load(f)
    if args.limit:
        queries = queries[:args.limit]

    if args.fresh:
        for p in (BENCH_CACHE, BENCH_FETCH):
            for suffix in ("", "-wal", "-shm"):
                if os.path.exists(p + suffix):
                    os.unlink(p + suffix)

    from fetchlib import Fetcher
    stats = {"bytes": 0, "fetches": 0, "robots_skips": 0}
    fetcher = Fetcher(cache_path=BENCH_FETCH)
    index = None
    if not args.no_index:
        from sites.index import SiteIndex
        index = SiteIndex(args.index)
    searcher = Searcher(cache=Cache(BENCH_CACHE), top_k=args.k,
                        discovery="curated" if args.curated else "registry",
                        stats=stats, fetcher=fetcher, index=index,
                        use_index=not args.no_index, page_budget_ms=args.page_budget)

    print("=" * 92)
    print(f"COLD CACHE — {len(queries)} queries — discovery={searcher.discovery}")
    print("=" * 92)
    cold = []
    cold_fetches = 0
    cold_bytes = 0
    for q in queries:
        res = await searcher.search(q["text"], k=args.k)
        hits = sum(1 for r in res["results"] if r["cache_hit"])
        cold.append(res["elapsed_ms"])
        fs = res.get("fetch_stats", {})
        cold_fetches += fs.get("total_fetches", 0)
        cold_bytes += fs.get("total_bytes", 0)
        print(f"  {q['id']} {q['text'].encode('ascii','ignore').decode()[:40]:40s} elapsed={res['elapsed_ms']:8.1f}ms  "
              f"results={len(res['results'])} hits={hits} fetch={fs.get('total_fetches', 0)}")

    print("\n" + "=" * 92)
    print("WARM CACHE — second run")
    print("=" * 92)
    warm = []
    sample = None
    for q in queries:
        res = await searcher.search(q["text"], k=args.k)
        if sample is None:
            sample = res
        hits = sum(1 for r in res["results"] if r["cache_hit"])
        warm.append(res["elapsed_ms"])
        print(f"  {q['id']} {q['text'].encode('ascii','ignore').decode()[:40]:40s} elapsed={res['elapsed_ms']:8.2f}ms  "
              f"results={len(res['results'])} hits={hits}")

    await searcher.close()
    await fetcher.close()
    if index is not None:
        index.close()

    c_med = statistics.median(cold)
    w_med = statistics.median(warm)
    c_p95 = sorted(cold)[min(len(cold) - 1, int(len(cold) * 0.95))]
    w_p95 = sorted(warm)[min(len(warm) - 1, int(len(warm) * 0.95))]

    print("\n" + "#" * 92)
    print("STAGE 3 BENCHMARK SUMMARY")
    print("#" * 92)
    print(f"queries: {len(queries)}   top_k={args.k}")
    print(f"cold:  median={c_med:.1f}ms  p95={c_p95:.1f}ms  fetches={cold_fetches}  bytes={cold_bytes/1e6:.2f} MB")
    print(f"warm:  median={w_med:.2f}ms  p95={w_p95:.2f}ms  fetches=0")
    print(f"warm-cache <10ms for all queries: {'PASS' if all(x < 10 for x in warm) else 'FAIL'}")

    if sample:
        print(f"\n--- sample payload ({queries[0]['id']}) ---")
        for r in sample["results"]:
            print(f"  url={r['url']}\n  source={r.get('source')} score={r['rank_score']:.4f} "
                  f"status={r.get('status')} cache_hit={r['cache_hit']} chars={len(r['clean_markdown'])}")


if __name__ == "__main__":
    asyncio.run(main())
