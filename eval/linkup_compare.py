"""Side-by-side Linkup comparison — stub, not run by the prototype.

The Kriko Magnet prototype never calls a search-engine provider. This script
exists so the owner can run the same judged queries against Linkup later and
compare numbers. It is inert unless you pass --run and set LINKUP_API_KEY.

Linkup pricing applies. Read https://docs.linkup.so before running.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
QUERIES = os.path.join(HERE, "queries.jsonl")
ENDPOINT = "https://api.linkup.so/v1/search"


def load_queries(path=QUERIES):
    with open(path, encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


def ndcg(flags, n_relevant, k):
    dcg = sum(rel / math.log2(i + 2) for i, rel in enumerate(flags[:k]))
    ideal = sum(1.0 / math.log2(i + 2) for i in range(min(k, n_relevant)))
    return dcg / ideal if ideal else 0.0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run", action="store_true", help="actually call Linkup (costs money)")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--k", type=int, default=5)
    ap.add_argument("--json", default="")
    args = ap.parse_args()

    queries = load_queries()
    if args.limit:
        queries = queries[:args.limit]

    print("Linkup side-by-side stub")
    print(f"  queries: {len(queries)}  endpoint: {ENDPOINT}")
    print("  Set LINKUP_API_KEY, then rerun with --run.")
    print("  This stub sends {q, depth, outputType} and reads results[].url,title,content.")
    print("  It is intentionally not executed by eval/run_eval.py.")

    if not args.run:
        print("\nDry run only. Nothing was sent to Linkup.")
        return

    key = os.environ.get("LINKUP_API_KEY")
    if not key:
        print("LINKUP_API_KEY is not set.", file=sys.stderr)
        sys.exit(2)

    import requests

    rows = []
    for q in queries:
        r = requests.post(
            ENDPOINT,
            headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
            json={"q": q["query"], "depth": "standard", "outputType": "searchResults"},
            timeout=60,
        )
        r.raise_for_status()
        data = r.json()
        results = data.get("results", data if isinstance(data, list) else [])
        urls = [item.get("url", "") for item in results[:args.k]]
        relevant = set(q.get("relevant", []))
        flags = [1 if u.rstrip("/") in {x.rstrip("/") for x in relevant} else 0 for u in urls]
        rows.append({
            "id": q["id"], "query": q["query"], "urls": urls, "flags": flags,
            "hit": 1 if any(flags) else 0,
            "ndcg": ndcg(flags, len(relevant), args.k),
        })
        print(f"  {q['id']} hit={rows[-1]['hit']} ndcg={rows[-1]['ndcg']:.3f}")

    judged = [r for r in rows if r["flags"] is not None]
    print(f"\nLinkup hit@5: {sum(r['hit'] for r in judged)/len(judged)*100:.1f}%  "
          f"nDCG@5: {sum(r['ndcg'] for r in judged)/len(judged):.3f}")
    if args.json:
        with open(args.json, "w", encoding="utf-8") as f:
            json.dump(rows, f, ensure_ascii=False, indent=1)


if __name__ == "__main__":
    main()
