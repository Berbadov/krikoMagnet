# Stage 3 — Minimal Full-Stack Pipeline

Wires Stage 1 (Rust ranker) + Stage 2 (async `curl_cffi` / `trafilatura`
worker) + SQLite cache, exposed as a single Python function and a minimal
FastAPI endpoint.

## Files
| File | Purpose |
|---|---|
| `gen_candidates.py` | Regenerates `candidates.jsonl` (26 real candidate URLs with scoring metadata) and `queries_pipeline.json` (10 benchmark queries) |
| `cache.py` | SQLite cache — `cache` table (url_hash, query_hash → markdown) and `qcache` table (query_hash → cached result payload) |
| `worker.py` | Async fetch (`curl_cffi`) + extract (`trafilatura`/`selectolax`); returns markdown, status, bytes, latency |
| `pipeline.py` | `Searcher`: query → Rust ranking → top-K → fetch/cache → structured results |
| `app.py` | FastAPI `POST /search` endpoint |
| `bench.py` | Cold vs warm 10-query benchmark (latency + bandwidth) |

## Request / response
`POST /search` body: `{"query": "...", "k": 3}`.

```json
{
  "query": "sqlite window functions tutorial",
  "elapsed_ms": 0.538,
  "results": [
    {
      "url": "https://sqlite.org/windowfunctions.html",
      "domain": "sqlite.org",
      "rank_score": 0.7947,
      "clean_markdown": "...",
      "latency_ms": 0.538,
      "cache_hit": true
    }
  ]
}
```

## Caching
- **Content cache:** keyed by `url_hash + query_hash`; stores the extracted
  markdown. Used on cold searches to avoid re-fetching.
- **Query-level cache:** stores the full result payload per `query_hash` so a
  repeated lookup returns without re-ranking or re-fetching. This is what makes
  warm lookups <10 ms (a per-URL cache hit still spent ~70 ms spawning Rust).

## Gate 3 result (PASS — warm cache <10 ms)
| | Cold | Warm |
|---|---|---|
| Median latency | 775 ms | **0.29 ms** |
| p95 latency | 1394 ms | **0.42 ms** |
| Bandwidth | 9.11 MB (30 fetches) | 0 B (0 fetches) |

## Run
```bash
python gen_candidates.py    # regenerate candidates/queries (optional)
python bench.py             # cold/warm benchmark
python app.py               # FastAPI service; then:
# curl -X POST localhost:8000/search -H 'Content-Type: application/json' \
#      -d '{"query":"postgresql jsonb partial index","k":3}'
```
