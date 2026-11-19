# Kriko Magnet — Search / Crawl / Extraction Prototype

Standalone, open-source prototype built in three strictly gated stages.
Every stage ran its benchmark, printed the metrics table, and only proceeded
past a gate that PASSED.

---

## Stage 1 — Zero-Cost Hybrid Ranker

**Signals (100% free):** BM25 over `title + snippet`, log-scaled Tranco top-1M
domain-authority prior, exponential freshness boost.

**Design notes**
- Tranco daily list downloaded via `curl_cffi` TLS impersonation (Cloudflare
  525 bypass) from the S3 origin; cached at `data/tranco_top100k.txt`.
- Synthetic corpus: 500 docs (60 relevant, 45 SEO-spam, 395 filler) across
  15 judged queries (product issues/reviews, technical docs, general).
- Scoring is *additive over a lexical gate*: only docs with ≥1 query-token
  match are candidates, so authority/freshness rescale real matches and never
  surface zero-match pages (fixed an early bug where apple.com/reddit.com
  floated into top-5 on priors alone). Weights tuned to `alpha=0.40,
  beta=0.60, gamma=0.15`.

### GATE 1A (Python prototype) — PASS
| Metric | Value | Gate |
|---|---|---|
| NDCG@5 (mean) | **0.910** | ≥ 0.80 ✅ |
| Top-5 Precision (mean) | 0.707 | — |
| Latency p50 / p95 | 20.8 / 33.1 ms | (speed gate is 1B) |

### GATE 1B (Rust port) — PASS
| Metric | Value | Gate |
|---|---|---|
| Full-rank parity vs Python (15/15 queries) | identical | correlation = 1.0 ✅ |
| Top-5 parity | identical | 1.0 ✅ |
| Latency p50 / p95 | **0.45 / 0.54 ms** | p95 ≤ 2 ms ✅ |

Rust port optimized by precomputing token arrays and inverted stats at load
(removed per-query re-tokenization) to meet the sub-2ms target.

---

## Stage 2 — Direct Fetching & Block-Rate Telemetry

40 real target URLs across news/review blogs, manufacturer support, technical
docs, forums, Reddit threads, iFixit guides, and product-review blogs.
Concurrency=8, `curl_cffi` (`impersonate="chrome"`), `trafilatura` extraction
with `selectolax` fallback. No proxies, no headless browser.

### GATE 2 — PASS
| Metric | Value | Gate |
|---|---|---|
| Clean extraction success rate | **80.0%** (32/40) | ≥ 75% ✅ |
| Hard/soft block rate | **17.5%** (7/40) | ≤ 25% ✅ |
| HTTP errors (non-block) | 1 (Ars Technica 405) | — |

**Blocked / degraded (honestly reported):**
- Hard block: `serverfault.com` → 403 (Cloudflare).
- Soft blocks (JS-shell / challenge, <300 clean chars):
  `support.hp.com` (0 chars), `www.quora.com` (118), `www.reddit.com` ×4 (144).

**Classifier accuracy fix:** challenge markers are matched only against the
*extracted text* (not raw JavaScript payloads), so fully-extracted pages
(apnews 4.9k, Lenovo 5.5k chars) are not false-flagged as soft-blocks.

---

## Stage 3 — Minimal Full-Stack Pipeline

Wires Stage 1 (Rust ranker) + Stage 2 (async `curl_cffi`/`trafilatura`
worker) + SQLite cache (keyed by URL hash + query hash), exposed as a single
Python function and `POST /search` FastAPI endpoint.

- Result JSON: `url`, `domain`, `rank_score`, `clean_markdown`, `latency_ms`,
  `cache_hit`.
- Query-level result cache (SQLite `qcache`) makes repeated lookups return
  without re-ranking or re-fetching, satisfying the <10ms warm requirement.

### End-to-end benchmark (10 queries, top-k=3)
| | Cold cache | Warm cache |
|---|---|---|
| Median latency | 775 ms | **0.29 ms** |
| p95 latency | 1394 ms | **0.42 ms** |
| Bandwidth | 9.11 MB (30 fetches) | 0 B (0 fetches) |
| Warm <10ms for all queries | — | **PASS** |

---

## Layout
All work lives under `kriko-magnet/`:
```
kriko-magnet/
  README.md          master overview (was KRIKO_MAGNET.md)
  RESULTS.md         gate metrics tables
  stage1_py/    Tranco cache, corpus generator, ranker, benchmark, tuning, parity
  stage1_rs/    Rust port (cargo) — ranker + --scores/--bench modes
  stage2_fetch/ URL roster, status check, fetch+telemetry benchmark
  stage3_pipeline/ cache, worker, pipeline, FastAPI app, cold/warm benchmark
  data/         tranco list, corpus, queries, sqlite cache
```
