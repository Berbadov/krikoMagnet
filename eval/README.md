# Evaluation

`queries.jsonl` contains 60 realistic Kriko queries: 59 with hand-checked
relevant URLs and one deliberately unjudged coverage gap (`anker a1257
recall`). Every judged URL returned HTTP 200 under the honest user agent when
checked on 2026-11-19/20. Turkish coverage was extended in Stage 5 with boiler
and appliance sources (Vaillant TR, ECA, Baymak, Bosch Home TR, ShiftDelete,
Donanım Arşivi).

## Run

```bash
python -m sites.index build          # one-time: fill the local sitemap index
python eval/run_eval.py --fresh
# optional:
python eval/run_eval.py --fresh --json eval/results_latest.json \
  --markdown eval/results_latest.md
```

`--fresh` deletes the eval query cache and the eval fetch cache, then runs:

1. **cold** — empty query cache, empty fetch cache; the local sitemap index is
   kept (that is the production warm-index path), so only top-k pages are
   fetched;
2. **warm-fetch, cold-query** — sitemap/page cache retained, query cache
   cleared;
3. **warm query cache** — exact repeats.

`--no-index` runs the old live-sitemap discovery for comparison.
`--page-budget MS` returns graceful partial results when page fetches exceed
the budget (0 = wait for all).

The script prints hit@5, nDCG@5, P@5, sources/query, result domains/query,
cold and warm latency, network fetches/query, robots skips, and block rate.
It does not call any search-engine provider.

## Judgments

A result counts only if its URL matches a judged URL (after lowercasing the
host, dropping `www.`, normalizing `http`/`https`, and stripping a trailing
slash). Near-miss pages count as misses; that is deliberate, so the metric is
not flattered by vaguely related results.

## Provider benchmark (Magnet vs DeepSeek Search vs Linkup)

`bench_providers.py` runs the same judged queries through Magnet, the DeepSeek
Search API (`POST /v1/tools/search`) and Linkup (`POST /v1/search`), scores the
top-5 URLs against these same exact-URL judgments, and reports hit@5, nDCG@5,
P@5, a softer domain-hit@5, latency percentiles, empty/error counts and cost.
Magnet runs with fresh caches (production index, first-run page fetches) and a
15 s page budget, so its row is the real user experience rather than a warm
cache.

```bash
export DEEPSEEK_API_KEY=...
export LINKUP_API_KEY=...
python eval/bench_providers.py --dry-run            # estimate spend, send nothing
python eval/bench_providers.py --limit 50           # writes bench_providers.{json,md}
```

It costs money: DeepSeek Search is $0.75 per 1,000 successful requests and
Linkup standard search is about $0.006 per query, so 50 queries on both is
roughly $0.35. `--limit` caps the query count; the default 50 stays under a
dollar. `linkup_compare.py` remains as the minimal Linkup-only stub.

Magnet's page-fetch budget defaults to 15 s (`--budget-ms`); a bigger budget
buys recall at the cost of tail latency, which is why the committed run also
includes a 30 s pass (`bench_magnet_30s.*`).
