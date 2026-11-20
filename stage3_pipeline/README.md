# Stage 3 — Full-Stack Pipeline

Wires Stage 1 (Rust ranker) + Stage 4 (site registry / query planning /
adapters) + Stage 2 (`fetchlib` polite fetch + trafilatura worker) + SQLite
cache, exposed as a Python function and a minimal FastAPI endpoint.

## Files
| File | Purpose |
|---|---|
| `pipeline.py` | `Searcher`: query -> planner/adapters (or curated fallback) -> Rust ranker -> top-K -> fetch/cache -> structured results |
| `worker.py` | Async fetch via `fetchlib` + extract (`trafilatura`, selectolax fallback) |
| `cache.py` | SQLite `cache` (url/query -> markdown) and `qcache` (query -> payload) tables |
| `app.py` | FastAPI `POST /search` endpoint |
| `bench.py` | Cold vs warm benchmark; registry mode by default, `--curated` fallback |
| `queries_kriko.json` | 8 representative registry-mode queries |
| `queries_pipeline.json` | Original 10 curated-mode benchmark queries |
| `gen_candidates.py` | Regenerates the old 26-URL `candidates.jsonl` fallback |

## Discovery modes
- **registry** (default): `sites.discover()` plans topics/language, picks sites
  from `sites/registry.yaml`, runs their adapters concurrently, merges and
  filters candidates, and sends the pool to the Rust ranker.
- **curated** (`discovery="curated"` / `bench.py --curated`): the original
  hand-picked 26-URL candidate list, kept for comparison with the old Stage 3
  numbers.

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
- **Content cache:** keyed by `url_hash + query_hash`; stores extracted
  markdown. Used to avoid re-extracting.
- **Query-level cache:** stores the full result payload per
  `discovery|k|query`; a repeat returns without discovery, ranking, or
  fetching. This is what keeps warm lookups at ~1 ms.
- **HTTP cache:** `fetchlib` caches raw responses with a TTL (default 24 h),
  plus robots.txt and parsed sitemaps.

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
