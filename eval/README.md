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

## Linkup comparison

`linkup_compare.py` is a stub and is not run by `run_eval.py`. It requires
`LINKUP_API_KEY` and `--run`, and it costs money:

```bash
export LINKUP_API_KEY=...
python eval/linkup_compare.py --run --json eval/linkup_results.json
```

Read https://docs.linkup.so for the current request/response schema before
using it.
