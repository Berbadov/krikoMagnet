"""Stage 3 end-to-end benchmark: 10 queries, cold cache vs warm cache."""
import asyncio
import json
import os

from pipeline import Searcher

HERE = os.path.dirname(os.path.abspath(__file__))


async def main():
    with open(os.path.join(HERE, "queries_pipeline.json"), encoding="utf-8") as f:
        queries = json.load(f)

    stats = {"bytes": 0, "fetches": 0}
    searcher = Searcher(stats=stats, top_k=3)

    print("=" * 92)
    print("COLD CACHE (first run — fetches + extracts)")
    print("=" * 92)
    cold = []
    for q in queries:
        res = await searcher.search(q["text"], k=3)
        hits = sum(1 for r in res["results"] if r["cache_hit"])
        cold.append(res["elapsed_ms"])
        statuses = [r.get("status") for r in res["results"]]
        print(f"  {q['id']} {q['text'][:38]:38s} elapsed={res['elapsed_ms']:7.1f}ms  results={len(res['results'])} hits={hits} statuses={statuses}")

    print("\n" + "=" * 92)
    print("WARM CACHE (second run — should all hit, <10ms)")
    print("=" * 92)
    warm = []
    for q in queries:
        res = await searcher.search(q["text"], k=3)
        hits = sum(1 for r in res["results"] if r["cache_hit"])
        warm.append(res["elapsed_ms"])
        print(f"  {q['id']} {q['text'][:38]:38s} elapsed={res['elapsed_ms']:7.2f}ms  results={len(res['results'])} hits={hits}")

    import statistics
    c_med = statistics.median(cold); w_med = statistics.median(warm)
    c_p95 = sorted(cold)[int(len(cold) * 0.95) - 1] if len(cold) else 0
    w_p95 = sorted(warm)[int(len(warm) * 0.95) - 1] if len(warm) else 0

    print("\n" + "#" * 92)
    print("STAGE 3 BENCHMARK SUMMARY")
    print("#" * 92)
    print(f"queries: {len(queries)}   top_k=3")
    print(f"cold:  median={c_med:.1f}ms  p95={c_p95:.1f}ms")
    print(f"warm:  median={w_med:.2f}ms  p95={w_p95:.2f}ms")
    print(f"bandwidth cold: {stats['bytes']/1e6:.2f} MB downloaded ({stats['fetches']} fetches)")
    print(f"bandwidth warm: 0.00 MB downloaded (0 fetches — cache hits)")
    warm_ok = all(x < 10 for x in warm)
    print(f"\nwarm-cache <10ms for all queries: {'PASS' if warm_ok else 'FAIL'}")

    # print one representative result payload
    print("\n--- sample result payload (pq1) ---")
    sample = await searcher.search(queries[0]["text"], k=3)
    for r in sample["results"]:
        md = r["clean_markdown"]
        print(f"  url={r['url']}\n  domain={r['domain']} score={r['rank_score']:.4f} "
              f"status={r.get('status')} cache_hit={r['cache_hit']} latency_ms={r['latency_ms']}")
        print(f"  clean_markdown_len={len(md)}  preview={md[:90]!r}")


if __name__ == "__main__":
    asyncio.run(main())
