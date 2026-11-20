"""Provider benchmark: Magnet vs DeepSeek Search vs Linkup.

Runs the same judged queries from eval/queries.jsonl through every provider,
scores the top-5 URLs against the existing exact-URL judgments (identical to
eval/run_eval.py) and reports hit@5, nDCG@5, P@5, latency and cost.

    python eval/bench_providers.py --limit 50 --providers all

Costs real money for the paid providers (DeepSeek $0.75/1k searches, Linkup
~$0.006/query). Use --dry-run to see the plan without sending anything.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import math
import os
import statistics
import sys
import tempfile
import time
from urllib.parse import unquote, urlsplit, urlunsplit

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
QUERIES = os.path.join(HERE, "queries.jsonl")
sys.path.insert(0, os.path.join(ROOT, "stage3_pipeline"))

DEEPSEEK_ENDPOINT = "https://api.deepseek.com/v1/tools/search"
LINKUP_ENDPOINT = "https://api.linkup.so/v1/search"
DEEPSEEK_USD_PER_SEARCH = 0.75 / 1000
LINKUP_USD_PER_SEARCH = 0.006
PAGE_BUDGET_MS = 15000


# --- scoring (same rules as eval/run_eval.py) -------------------------------

def normalize_url(url: str) -> str:
    parts = urlsplit((url or "").strip())
    path = unquote(parts.path or "/")
    if path != "/" and path.endswith("/"):
        path = path[:-1]
    scheme = "https" if parts.scheme in ("http", "https") else parts.scheme
    host = parts.netloc.lower()
    if host.startswith("www."):
        host = host[4:]
    return urlunsplit((scheme, host, path, parts.query, ""))


def domain_of(url: str) -> str:
    host = urlsplit(url or "").netloc.lower()
    return host[4:] if host.startswith("www.") else host


def dcg(flags, k):
    return sum(rel / math.log2(i + 2) for i, rel in enumerate(flags[:k]))


def ndcg(flags, n_relevant, k):
    ideal = sum(1.0 / math.log2(i + 2) for i in range(min(k, n_relevant)))
    return dcg(flags, k) / ideal if ideal else 0.0


def score_row(query, urls, k):
    relevant = {normalize_url(u) for u in query.get("relevant", [])}
    flags = [1 if normalize_url(u) in relevant else 0 for u in urls[:k]]
    domains = {domain_of(u) for u in relevant}
    return {
        "urls": urls,
        "flags": flags,
        "hit": 1 if any(flags) else 0,
        "precision": sum(flags) / k if k else 0.0,
        "ndcg": ndcg(flags, len(relevant), k) if relevant else 0.0,
        "domain_hit": 1 if domains & {domain_of(u) for u in urls[:k]} else 0,
        "judged": bool(relevant),
    }


def load_queries(limit):
    out = []
    with open(QUERIES, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                out.append(json.loads(line))
    return out[:limit] if limit else out


# --- providers --------------------------------------------------------------

async def run_magnet(queries, k, budget_ms):
    """Magnet with fresh caches, i.e. the first run of a session."""
    from pipeline import Searcher
    from cache import Cache
    from fetchlib import Fetcher

    tmp = tempfile.mkdtemp(prefix="magnet-bench-")
    fetch_cache = os.path.join(tmp, "fetch.sqlite")
    query_cache = os.path.join(tmp, "query.sqlite")
    fetcher = Fetcher(cache_path=fetch_cache)
    searcher = Searcher(cache=Cache(path=query_cache), fetcher=fetcher,
                        registry=None, page_budget_ms=budget_ms)
    rows = []
    try:
        for i, q in enumerate(queries, 1):
            before = dict(fetcher.stats)
            t0 = time.perf_counter()
            error = None
            try:
                payload = await searcher.search(q["query"], k=k)
                results = payload.get("results", [])
            except Exception as exc:
                results, error = [], repr(exc)[:200]
            ms = (time.perf_counter() - t0) * 1000
            row = {
                "id": q["id"], "query": q["query"], "provider": "magnet",
                "language": q.get("language", ""), "ms": round(ms, 1),
                "results": len(results), "error": error, "cost_usd": 0.0,
                "fetches": fetcher.stats["requests"] - before["requests"],
                "bytes": fetcher.stats["bytes"] - before["bytes"],
            }
            row.update(score_row(q, [r.get("url", "") for r in results], k))
            rows.append(row)
            print(f"  magnet   [{i:02d}/{len(queries)}] {q['id']} "
                  f"hit={row['hit']} ndcg={row['ndcg']:.2f} {row['ms']:.0f}ms", flush=True)
    finally:
        await searcher.close()
    return rows


def _post(url, headers, payload, timeout):
    import requests

    last = None
    for attempt in range(3):
        try:
            r = requests.post(url, headers=headers, json=payload, timeout=timeout)
            if r.status_code in (429, 500, 502, 503, 504):
                last = f"HTTP {r.status_code}"
                time.sleep(1.5 * (attempt + 1))
                continue
            r.raise_for_status()
            return r.json(), None
        except Exception as exc:
            last = repr(exc)[:200]
            time.sleep(1.0 * (attempt + 1))
    return None, last


def run_deepseek(queries, k):
    key = os.environ.get("DEEPSEEK_API_KEY")
    if not key:
        raise SystemExit("DEEPSEEK_API_KEY is not set")
    headers = {"Authorization": f"Bearer {key}", "Content-Type": "application/json"}
    rows = []
    for i, q in enumerate(queries, 1):
        payload = {"query": q["query"], "count": k, "need_content": True}
        t0 = time.perf_counter()
        data, error = _post(DEEPSEEK_ENDPOINT, headers, payload, 90)
        ms = (time.perf_counter() - t0) * 1000
        results = (data or {}).get("results", [])
        row = {
            "id": q["id"], "query": q["query"], "provider": "deepseek",
            "language": q.get("language", ""), "ms": round(ms, 1),
            "results": len(results), "error": error,
            "cost_usd": 0.0 if error or not results else DEEPSEEK_USD_PER_SEARCH,
            "content_chars": sum(len(x.get("content") or "") for x in results),
        }
        row.update(score_row(q, [x.get("url", "") for x in results], k))
        rows.append(row)
        print(f"  deepseek [{i:02d}/{len(queries)}] {q['id']} "
              f"hit={row['hit']} ndcg={row['ndcg']:.2f} {row['ms']:.0f}ms", flush=True)
    return rows


def run_linkup(queries, k):
    key = os.environ.get("LINKUP_API_KEY")
    if not key:
        raise SystemExit("LINKUP_API_KEY is not set")
    headers = {"Authorization": f"Bearer {key}", "Content-Type": "application/json"}
    rows = []
    for i, q in enumerate(queries, 1):
        payload = {"q": q["query"], "depth": "standard", "outputType": "searchResults"}
        t0 = time.perf_counter()
        data, error = _post(LINKUP_ENDPOINT, headers, payload, 120)
        ms = (time.perf_counter() - t0) * 1000
        results = (data or {}).get("results", []) if isinstance(data, dict) else (data or [])
        row = {
            "id": q["id"], "query": q["query"], "provider": "linkup",
            "language": q.get("language", ""), "ms": round(ms, 1),
            "results": len(results), "returned": len(results), "error": error,
            "cost_usd": 0.0 if error or not results else LINKUP_USD_PER_SEARCH,
            "content_chars": sum(len(x.get("content") or "") for x in results),
        }
        row.update(score_row(q, [x.get("url", "") for x in results], k))
        rows.append(row)
        print(f"  linkup   [{i:02d}/{len(queries)}] {q['id']} "
              f"hit={row['hit']} ndcg={row['ndcg']:.2f} {row['ms']:.0f}ms", flush=True)
    return rows


# --- reporting --------------------------------------------------------------

def summarize(rows, k, label):
    judged = [r for r in rows if r.get("judged")]
    errors = [r for r in rows if r.get("error")]
    lat = [r["ms"] for r in rows if not r.get("error")]
    lat.sort()

    def pct(p):
        return lat[min(len(lat) - 1, int(round(p / 100 * (len(lat) - 1))))] if lat else 0.0

    return {
        "provider": label,
        "queries": len(rows),
        "judged": len(judged),
        "errors": len(errors),
        "empty": sum(1 for r in rows if not r.get("results")),
        "hit@5": statistics.mean(r["hit"] for r in judged) if judged else 0.0,
        "ndcg@5": statistics.mean(r["ndcg"] for r in judged) if judged else 0.0,
        "p@5": statistics.mean(r["precision"] for r in judged) if judged else 0.0,
        "domain_hit@5": statistics.mean(r["domain_hit"] for r in judged) if judged else 0.0,
        "p50_ms": round(statistics.median(lat), 1) if lat else 0.0,
        "p95_ms": round(pct(95), 1),
        "cost_usd": round(sum(r.get("cost_usd", 0.0) for r in rows), 4),
    }


def markdown(summaries, rows, k, limit, providers, budget_ms=PAGE_BUDGET_MS):
    by = {s["provider"]: s for s in summaries}
    lines = []
    names = {"magnet": "Magnet", "deepseek": "DeepSeek Search", "linkup": "Linkup"}
    lines.append("# " + " vs ".join(names.get(p, p) for p in providers) + "\n")
    lines.append(f"- Queries: {limit} drawn from `eval/queries.jsonl` "
                 f"({summaries[0]['judged']} with exact-URL judgments)")
    lines.append(f"- Cutoff: top-{k}; judgments are the existing Kriko ones, "
                 "matched exactly after URL normalisation (no LLM judging)")
    lines.append("- Providers: " + ", ".join(
        {"magnet": "Magnet (this repo, fresh caches)",
         "deepseek": "DeepSeek Search API (`/v1/tools/search`, `need_content:true`)",
         "linkup": "Linkup `/v1/search` (`depth=standard`, first 5 of the returned list)"}
        .get(p, p) for p in providers))
    lines.append("")
    lines.append("## Headline\n")
    lines.append("| Provider | hit@5 | nDCG@5 | P@5 | domain-hit@5 | p50 | p95 | empty | errors | cost | $/hit |")
    lines.append("|---|---|---|---|---|---|---|---|---|---|---|")
    for s in summaries:
        hits = sum(r["hit"] for r in rows[s["provider"]] if r.get("judged"))
        per_hit = f"${s['cost_usd']/hits:.3f}" if hits else "—"
        lines.append(
            f"| {s['provider']} | {s['hit@5']*100:.1f}% | {s['ndcg@5']:.3f} | "
            f"{s['p@5']:.2f} | {s['domain_hit@5']*100:.1f}% | {s['p50_ms']:.0f} ms | "
            f"{s['p95_ms']:.0f} ms | {s['empty']} | {s['errors']} | "
            f"${s['cost_usd']:.4f} | {per_hit} |")
    lines.append("")
    lines.append("*domain-hit@5 is a softer secondary signal: a judged domain "
                 "appears in the top 5 even when the exact URL differs.*\n")

    if "magnet" in rows:
        magnet_rows = rows["magnet"]
        lines.append(
            "Magnet's network cost in this run: "
            f"{statistics.mean(r.get('fetches', 0) for r in magnet_rows):.1f} fetches and "
            f"{statistics.mean(r.get('bytes', 0) for r in magnet_rows)/1e3:.0f} KB per query, "
            f"{sum(r.get('bytes', 0) for r in magnet_rows)/1e6:.1f} MB in total, "
            f"{statistics.mean(r['results'] for r in magnet_rows):.1f} results per query.\n")

    lines.append("## Reading these numbers\n")
    lines.append("- **Home field.** The judgments are Kriko's own: exact URLs on "
                 "review, complaint, recall and repair sites that the registry was "
                 "built around. The paid engines are asked to land on the exact page "
                 "a curated source list was designed to produce, the hardest framing "
                 "for them. Read this as a domain benchmark, not a general web-search "
                 "ranking.")
    lines.append("- **Exact URLs only.** A near-miss page scores zero, so "
                 "`domain-hit@5` sits next to it: a provider that returns the right "
                 "site but the wrong article still shows up there.")
    lines.append("- **Latency is not like-for-like.** Magnet pays for robots.txt, "
                 "per-host spacing and page extraction on a cold run; the paid APIs "
                 "return prepared snippets. Magnet's p50 is seconds, theirs "
                 "sub-second.")
    lines.append("- **Cost is list price, one call per query.** Magnet's $0 is "
                 "self-hosted compute and bandwidth, not free — the run reports its "
                 "own fetch and byte counts above.\n")

    lines.append("## By language\n")
    lines.append("| Provider | set | hit@5 | nDCG@5 | p50 |")
    lines.append("|---|---|---|---|---|")
    for provider in providers:
        for lang in ("en", "tr"):
            subset = [r for r in rows[provider] if r["language"] == lang and r.get("judged")]
            if not subset:
                continue
            lines.append(
                f"| {provider} | {lang} ({len(subset)}) | "
                f"{statistics.mean(r['hit'] for r in subset)*100:.1f}% | "
                f"{statistics.mean(r['ndcg'] for r in subset):.3f} | "
                f"{statistics.median(r['ms'] for r in subset):.0f} ms |")
    lines.append("")

    lines.append("## Head-to-head (hit@5, per query)\n")
    base = {r["id"]: r for r in rows[providers[0]]}
    for other in providers[1:]:
        wins = ties = losses = 0
        for r in rows[other]:
            a = base.get(r["id"])
            if not a or not a.get("judged"):
                continue
            if a["hit"] > r["hit"]:
                wins += 1
            elif a["hit"] < r["hit"]:
                losses += 1
            else:
                ties += 1
        lines.append(f"- {providers[0]} vs {other}: **{wins} W / {ties} T / {losses} L**")
    lines.append("")

    lines.append("## Method\n")
    lines.append("- Same query text, same k=5, one live request per query per provider.")
    lines.append("- Magnet runs the actual pipeline (local SQLite FTS index, planner, "
                 f"Rust ranker, polite fetch with a {int(budget_ms)/1000:.0f} s page budget) "
                 "using fresh caches, "
                 "so its numbers are a first-run-in-a-session, not a warm cache.")
    lines.append("- DeepSeek and Linkup are called directly, one query at a time, with "
                 "up to three attempts on 429/5xx; failures are counted, not hidden.")
    lines.append("- Cost uses list prices captured with the run: DeepSeek Search "
                 "$0.75/1k successful requests, Linkup standard search $0.006/request, "
                 "Magnet $0 (self-hosted; the run reports its own fetch and byte counts).")
    lines.append("")

    lines.append("## Misses (no judged URL in top 5)\n")
    for provider in providers:
        misses = [r for r in rows[provider] if r.get("judged") and not r["hit"]]
        lines.append(f"### {provider} ({len(misses)})\n")
        for r in misses[:15]:
            got = ", ".join(domain_of(u) or "-" for u in r["urls"][:3]) or "no results"
            lines.append(f"- `{r['id']}` {r['query']} → {got}")
        if len(misses) > 15:
            lines.append(f"- … {len(misses) - 15} more")
        lines.append("")
    return "\n".join(lines)


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int, default=50)
    ap.add_argument("--k", type=int, default=5)
    ap.add_argument("--providers", default="all")
    ap.add_argument("--json", default=os.path.join(HERE, "bench_providers.json"))
    ap.add_argument("--markdown", default=os.path.join(HERE, "bench_providers.md"))
    ap.add_argument("--budget-ms", type=int, default=PAGE_BUDGET_MS,
                    help="Magnet page-fetch budget in ms (0 = wait for all)")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    providers = (["magnet", "deepseek", "linkup"] if args.providers == "all"
                 else [p.strip() for p in args.providers.split(",") if p.strip()])
    queries = load_queries(args.limit)
    print(f"{len(queries)} queries, top-{args.k}, providers={providers}")
    if args.dry_run:
        est = (len(queries) * DEEPSEEK_USD_PER_SEARCH * ("deepseek" in providers)
               + len(queries) * LINKUP_USD_PER_SEARCH * ("linkup" in providers))
        print(f"estimated spend: ${est:.2f} (no requests sent)")
        return

    rows, summaries = {}, []
    t0 = time.time()
    if "magnet" in providers:
        print("magnet...", flush=True)
        rows["magnet"] = await run_magnet(queries, args.k, args.budget_ms)
    if "deepseek" in providers:
        print("deepseek...", flush=True)
        rows["deepseek"] = await asyncio.to_thread(run_deepseek, queries, args.k)
    if "linkup" in providers:
        print("linkup...", flush=True)
        rows["linkup"] = await asyncio.to_thread(run_linkup, queries, args.k)

    for provider in providers:
        summaries.append(summarize(rows[provider], args.k, provider))

    with open(args.json, "w", encoding="utf-8") as f:
        json.dump({"k": args.k, "limit": args.limit,
                   "budget_ms": args.budget_ms,
                   "elapsed_s": round(time.time() - t0, 1),
                   "summaries": summaries, "rows": rows}, f, ensure_ascii=False, indent=1)
    report = markdown(summaries, rows, args.k, len(queries), providers,
                      args.budget_ms)
    with open(args.markdown, "w", encoding="utf-8") as f:
        f.write(report + "\n")

    print("\n" + report[:2500])
    print(f"\nwrote {args.json} and {args.markdown} in {time.time() - t0:.0f}s")


if __name__ == "__main__":
    asyncio.run(main())
