# Stage 3 — Full-Stack Pipeline

Wires Stage 1 (Rust ranker) + Stage 4 (site registry / query planning /
adapters) + Stage 2 (`fetchlib` polite fetch + trafilatura worker) + SQLite
cache, exposed as a Python function and a minimal FastAPI endpoint.

## Files
| File | Purpose |
|---|---|
| `pipeline.py` | `Searcher`: query -> planner/adapters or local index -> resident Rust ranker -> top-K -> fetch/cache -> structured results |
| `ranker.py` | Resident Rust ranker (`--serve`, JSON lines) with one-shot fallback |
| `worker.py` | Async fetch via `fetchlib` + extract (`trafilatura`, selectolax fallback); optional page budget |
| `cache.py` | SQLite (WAL) `cache` and `qcache` tables with TTLs |
| `app.py` | FastAPI `POST /search`, `/health` (index + circuit), optional background index refresh |
| `bench.py` | Cold vs warm benchmark; registry mode by default, `--curated` fallback |
| `queries_kriko.json` | 8 representative registry-mode queries |
| `queries_pipeline.json` | Original 10 curated-mode benchmark queries |
| `gen_candidates.py` | Regenerates the old 26-URL `candidates.jsonl` fallback |

## Discovery modes
- **registry** (default): `sites.discover()` plans topics/language, picks sites
  from `sites/registry.yaml`, asks the local index for sitemap/RSS sites and
  live adapters for search/API sites, merges and filters candidates, and sends
  the pool to the Rust ranker.
- **curated** (`discovery="curated"` / `bench.py --curated`): the original
  hand-picked 26-URL candidate list, kept for comparison with the old Stage 3
  numbers.

## Speed path (Stage 5)
1. Build the index once: `python -m sites.index build` (polite, full backfill).
2. Query: local FTS5 lookup (~1 ms) instead of sitemap downloads.
3. Resident ranker: the Rust binary loads Tranco once and answers JSON lines;
   a per-query rank is ~2 ms instead of a ~70 ms process spawn.
4. Fetch only top-k pages concurrently across hosts, still >=1 s apart per
   host. `--page-budget MS` returns graceful partial results when a slow host
   would exceed the budget (0 = wait for all).

## Request / response
`POST /search` body: `{"query": "...", "k": 3}`.

```json
{
  "query": "lg c1 burn in",
  "elapsed_ms": 0.65,
  "plan": {
    "language": "en",
    "topics": ["tv-audio", "appliances", "phones"],
    "sites": ["rtings", "whathifi", "..."],
    "sources_with_results": 1
  },
  "fetch_stats": {
    "discovery_fetches": 0,
    "page_fetches": 0,
    "total_fetches": 0,
    "robots_skips": 0
  },
  "results": [
    {
      "url": "https://www.rtings.com/tv/reviews/lg/c1-oled",
      "domain": "www.rtings.com",
      "title": "tv reviews lg c1 oled",
      "snippet": "rtings tv reviews lg c1 oled",
      "source": "rtings",
      "method": "sitemap",
      "rank_score": 0.8671,
      "clean_markdown": "...",
      "status": 200,
      "latency_ms": 0.65,
      "cache_hit": true
    }
  ]
}
```

## Caching
- **Local index:** `data/site_index.sqlite` (FTS5) holds sitemap/RSS page
  metadata. `python -m sites.index refresh` does a conditional refresh;
  `python -m sites.index stats` prints counts and age.
- **Content cache:** keyed by `url_hash + query_hash`; stores extracted
  markdown (7-day TTL).
- **Query-level cache:** stores the full result payload per
  `discovery|k|query` (6-hour TTL); a repeat returns without discovery,
  ranking, or fetching. This is what keeps warm lookups at ~1 ms.
- **HTTP cache:** `fetchlib` caches raw responses with a TTL (default 24 h),
  plus robots.txt and parsed sitemaps; conditional GETs refresh sitemaps.

## Benchmarks
Registry mode, 8 Kriko queries, top-k=3:

| | Cold | Warm |
|---|---|---|
| Median latency | 8904 ms | **0.65 ms** |
| p95 latency | 14241 ms | **0.92 ms** |
| Network fetches | 99 | 0 |
| Bandwidth | 36.86 MB | 0 B |
| Warm <10 ms for all queries | — | **PASS** |

The old curated fallback (10 queries, top-k=3) remains at 775 ms cold /
0.29 ms warm.

## Run
```bash
python stage3_pipeline/bench.py --fresh      # registry mode
python stage3_pipeline/bench.py --curated --fresh
python stage3_pipeline/app.py                # http://127.0.0.1:8000
curl -X POST localhost:8000/search -H 'Content-Type: application/json' \
     -d '{"query":"lg c1 burn in","k":3}'
```
