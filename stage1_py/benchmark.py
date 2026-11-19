"""Stage 1A: Top-5 precision / NDCG@5 + p50/p95 latency for the hybrid ranker."""
import json
import math
import os
import statistics
import time

from ranker import Ranker

NDCG_K = 5
REPEATS = 30  # latency measurement iterations per query


def dcg_at_k(relevance, k=NDCG_K):
    return sum(rel / math.log2(i + 2) for i, rel in enumerate(relevance[:k]))


def ndcg(relevance, k=NDCG_K, n_relevant=None):
    actual = dcg_at_k(relevance, k)
    n_rel = n_relevant if n_relevant is not None else sum(relevance)
    ideal = sum(1.0 / math.log2(i + 2) for i in range(min(k, n_rel)))
    return actual / ideal if ideal > 0 else 0.0


def run_benchmark(weights=None):
    r = Ranker(weights=weights)
    r.load_corpus()
    queries = r.load_queries()
    good_set = {q["id"]: set(q["relevant"]) for q in queries}

    rows = []
    precisions, ndcgs = [], []
    lat_samples = []

    for q in queries:
        rel = good_set[q["id"]]
        # measure latency over REPEATS full-rank runs
        t0 = time.perf_counter()
        for _ in range(REPEATS):
            r.rank(q["text"], top_k=NDCG_K)
        elapsed = (time.perf_counter() - t0) / REPEATS
        lat_samples.append(elapsed)

        ranked = r.rank(q["text"], top_k=NDCG_K)
        rel_flags = [1 if d["id"] in rel else 0 for d in ranked]
        p = sum(rel_flags) / NDCG_K
        n = ndcg(rel_flags, NDCG_K, len(rel))
        precisions.append(p)
        ndcgs.append(n)
        rows.append((q["id"], q["category"], q["text"], len(rel), p, n, elapsed * 1000))

    # metrics table
    print("=" * 100)
    print(f"{'qid':<5}{'category':<9}{'query':<38}{'rel':<4}{'P@5':<7}{'NDCG@5':<9}{'lat_ms':<8}")
    print("-" * 100)
    for qid, cat, text, nrel, p, n, lat in rows:
        print(f"{qid:<5}{cat:<9}{text[:38]:<38}{nrel:<4}{p:<7.3f}{n:<9.3f}{lat:<8.3f}")
    print("-" * 100)
    mean_p = statistics.mean(precisions)
    mean_ndcg = statistics.mean(ndcgs)
    print(f"MEAN  {'':<9}{'':<38}{'':<4}{mean_p:<7.3f}{mean_ndcg:<9.3f}")
    print("=" * 100)
    print(f"Top-5 Precision (mean): {mean_p:.4f}")
    print(f"NDCG@5 (mean):         {mean_ndcg:.4f}  (GATE 1A requires >= 0.80)")
    print(f"Latency p50: {statistics.median(lat_samples)*1000:.3f} ms  |  p95: {sorted(lat_samples)[int(len(lat_samples)*0.95)-1]*1000:.3f} ms")
    return mean_ndcg, mean_p


if __name__ == "__main__":
    import sys
    weights = None
    if len(sys.argv) > 1:
        weights = json.loads(sys.argv[1])
    run_benchmark(weights)
