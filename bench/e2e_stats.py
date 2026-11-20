import json, statistics

rows = [json.loads(l) for l in open("bench/out/e2e.jsonl", encoding="utf-8")]
cold = [r for r in rows if r["phase"] == "cold" and r["judged"]]
zero = [r for r in cold if len(r["results"]) == 0]
print("cold", len(cold), "zero-result", len(zero))
print("mean scraped", round(statistics.mean(len(r["results"]) for r in cold), 2),
      "mean urls", round(statistics.mean(len(r["search_urls"]) for r in cold), 2),
      "mean denied", round(statistics.mean(len(r["denied"]) for r in cold), 2))
print("scrape fail", sum(1 for r in cold for x in r["results"] if not x["ok"]),
      "of", sum(len(r["results"]) for r in cold))
print("empty search", sum(1 for r in cold if not r["search_urls"]))
print("queries with search error", sum(1 for r in cold if r.get("search_info", {}).get("error")))
print("total_ms p50/p95", round(statistics.median(r["total_ms"] for r in cold)),
      round(sorted(r["total_ms"] for r in cold)[int(len(cold) * .95)]))
print("search p50/p95", round(statistics.median(r["search_ms"] for r in cold)),
      round(sorted(r["search_ms"] for r in cold)[int(len(cold) * .95)]))
print("slowest", sorted(((round(r["total_ms"] / 1000), r["id"], r["query"]) for r in cold), reverse=True)[:5])
print("hits", sum(r["hit"] for r in cold))
byq = {r["id"]: (r["hit"], round(r["ndcg"], 2), len(r["denied"])) for r in cold}
print(byq)
warm = [r for r in rows if r["phase"] == "warm"]
print("warm total p50", round(statistics.median(r["total_ms"] for r in warm)))
