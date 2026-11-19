import json
import math
import statistics
from ranker import Ranker

SETS = [
    {"alpha": 0.40, "beta": 0.45, "gamma": 0.15},
    {"alpha": 0.40, "beta": 0.60, "gamma": 0.15},
    {"alpha": 0.40, "beta": 0.70, "gamma": 0.15},
    {"alpha": 0.40, "beta": 0.85, "gamma": 0.15},
    {"alpha": 0.45, "beta": 0.60, "gamma": 0.15},
    {"alpha": 0.35, "beta": 0.80, "gamma": 0.15},
    {"alpha": 0.40, "beta": 0.75, "gamma": 0.10},
    {"alpha": 0.40, "beta": 0.80, "gamma": 0.10},
    {"alpha": 0.50, "beta": 0.65, "gamma": 0.10},
]


def ndcg(rel_flags, k=5, n_rel=None):
    actual = sum(f / math.log2(i + 2) for i, f in enumerate(rel_flags[:k]))
    ideal = sum(1 / math.log2(i + 2) for i in range(min(k, n_rel)))
    return actual / ideal if ideal > 0 else 0.0


for w in SETS:
    r = Ranker(weights=w)
    r.load_corpus()
    queries = r.load_queries()
    nds, ps = [], []
    for q in queries:
        rel = set(q["relevant"])
        ranked = r.rank(q["text"], top_k=5)
        flags = [1 if d["id"] in rel else 0 for d in ranked]
        ps.append(sum(flags) / 5)
        nds.append(ndcg(flags, 5, len(rel)))
    print(f"{w}  ->  P@5={statistics.mean(ps):.3f}  NDCG@5={statistics.mean(nds):.3f}  minNDCG={min(nds):.3f}")
