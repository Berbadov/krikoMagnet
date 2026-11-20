# Kriko Magnet — Results

Standalone, free-signal search prototype. No search-engine provider is used
for discovery: pages come from site-owned search pages, sitemaps, RSS feeds,
and one official API, and are ranked by the Rust BM25 + Tranco prior +
freshness ranker.

Version: v0.2.0 (2026-11-20).

---

## Stage 1 — Zero-Cost Hybrid Ranker

**Signals (100% free):** BM25 over `title + snippet`, log-scaled Tranco top-1M
domain-authority prior, exponential freshness boost.

**Design notes**
- Tranco daily list downloaded from the **S3 origin**
  (`tranco-list.s3.amazonaws.com`) with an honest user agent; cached at
  `data/tranco_top100k.txt`. The old `tranco-list.eu` 525 was an SSL handshake
  failure, not a bot wall; no TLS impersonation is used.
- Synthetic corpus: 500 docs (60 relevant, 45 SEO-spam, 395 filler) across
  15 judged queries (product issues/reviews, technical docs, general).
- Scoring is *additive over a lexical gate*: only docs with >=1 query-token
  match are candidates, so authority/freshness rescale real matches and never
  surface zero-match pages. Weights tuned to `alpha=0.40, beta=0.60,
  gamma=0.15`.

### GATE 1A (Python prototype) — PASS
| Metric | Value | Gate |
|---|---|---|
| NDCG@5 (mean) | **0.910** | >= 0.80 PASS |
| Top-5 Precision (mean) | 0.707 | — |
| Latency p50 / p95 | 20.8 / 33.1 ms | speed gate is 1B |

### GATE 1B (Rust port) — PASS
| Metric | Value | Gate |
|---|---|---|
| Full-rank parity vs Python (15/15 queries) | identical | correlation = 1.0 PASS |
| Top-5 parity | identical | 1.0 PASS |
| Latency p50 / p95 | **0.45 / 0.54 ms** | p95 <= 2 ms PASS |

The Rust port precomputes token arrays and inverted stats at load, removing
per-query re-tokenization. Stage 3/4 add an optional `discovery` field to the
Rust corpus; it defaults to 0, so this parity/latency result is unchanged.

---

## Stage 2 — Polite Direct Fetching & Block-Rate Telemetry

40 real target URLs across news/review blogs, manufacturer support, technical
docs, forums, Reddit threads, iFixit guides, and product-review blogs.
Fetched through the shared `fetchlib` layer: robots.txt + honest UA
(`KrikoMagnet/0.2 (+https://kriko.cc/bot)`), per-host >=1 s spacing, global
concurrency 8, 429/Retry-After backoff, no proxies, no headless browser, no
Chrome impersonation.

### GATE 2 — PASS
| Metric | Value | Gate |
|---|---|---|
| Clean extraction success (fetchable targets) | **81.8%** (27/33) | >= 75% PASS |
| Hard/soft block rate (fetchable targets) | **15.2%** (5/33) | <= 25% PASS |
| HTTP errors (non-block) | 1 (Ars Technica 405) | — |
| robots.txt skips (never fetched) | 7 | reported separately |

**Blocked / skipped (honest):**
- Hard blocks: `apnews.com`, `realpython.com`, `www.quora.com`,
  `www.sitepoint.com` → 403.
- Soft block: `forums.tomshardware.com` (89 clean chars).
- HTTP error: `arstechnica.com` → 405.
- robots.txt disallowed: `serverfault.com`, `stackexchange.com`,
  `www.lenovo.com`, `www.reddit.com` ×4 — skipped, not fetched.

The old Stage-2 run (80.0% / 17.5%) used Chrome impersonation and treated
Reddit challenge pages as fetches. The polite run above is the current number.

---

## Stage 3 — Full-Stack Pipeline

Wires Stage 1 (Rust ranker), registry discovery (Stage 4), Stage 2
(`fetchlib` + trafilatura worker), and a SQLite cache, exposed as a Python
function and `POST /search` FastAPI endpoint.

- Result JSON: `url`, `domain`, `title`, `snippet`, `published`, `source`,
  `method`, `rank_score`, `clean_markdown`, `status`, `latency_ms`,
  `cache_hit`.
- The payload also carries the query plan (`language`, `topics`, `sites`,
  `sources_with_results`) and fetch stats (discovery/page/total fetches,
  bytes, robots skips).

### Registry mode (8 Kriko queries, top-k=3)
| | Cold cache | Warm cache |
|---|---|---|
| Median latency | 8904 ms | **0.65 ms** |
| p95 latency | 14241 ms | **0.92 ms** |
| Network fetches | 99 | 0 |
| Bandwidth | 36.86 MB | 0 B |
| Warm <10ms for all queries | — | **PASS** |

Cold includes sitemap downloads and discovery; warm is the query-level cache
hit. A representative cold result for `lg c1 burn in` is the RTINGS LG C1
review plus its settings page.

### Old hand-curated fallback (10 queries, top-k=3)
Kept only for comparison (`python stage3_pipeline/bench.py --curated`):

| | Cold | Warm |
|---|---|---|
| Median latency | 775 ms | 0.29 ms |
| p95 latency | 1394 ms | 0.42 ms |
| Bandwidth | 9.11 MB (30 fetches) | 0 B |

---

## Stage 4 — Query -> Pages Without a Search Engine

### What was built
- A YAML registry of **14 active sites** with topics, language/region, and a
  verified discovery method (site search page, sitemap/RSS, or official API).
  Sites whose robots.txt disallows the needed path are recorded as skipped.
- A keyword/language query planner (no LLM), per-site adapters, concurrent
  discovery within politeness limits, candidate merging, and deterministic
  model/brand/product/intent filters.
- A shared polite fetch layer (`fetchlib`) used by Stage 2, Stage 3, and all
  adapters.
- An adapter health check and 23 pytest tests (robots compliance, rate
  limiting, adapter parsing, query planning).

### Evaluation design
- **42 realistic Kriko queries**: 27 English + 15 Turkish. 41 are judged;
  one (`anker a1257 recall`) is a documented coverage gap and is not scored.
- **83 judged URL references (75 unique URLs)**, hand-checked by opening each
  page on 2026-11-19; every judged URL returned HTTP 200 under the honest user
  agent.
- Relevance is exact-URL after host/trailing-slash normalization. Near-miss
  pages count as misses.
- `cold` = empty query cache and empty fetch cache at the start of the run
  (sitemaps/downloads warm later queries in the same run); `warm-fetch` =
  sitemaps/pages cached, query cache cleared; `warm` = exact repeat.
- No search-engine provider was called. Linkup was not called.

### GATE 4 — results (k=5)
| Metric | Value |
|---|---|
| Judged queries | 41 |
| Queries with >=1 relevant result in top-5 | **92.7%** (38/41) |
| nDCG@5 (mean) | **0.763** |
| Precision@5 (mean) | 0.307 |
| Sources returning candidates / query (mean) | 1.71 |
| Result domains / query (mean) | 1.24 |
| Cold latency p50 / p95 | 6462 / 10627 ms |
| Warm-fetch, cold-query p50 / p95 | 1001 / 2766 ms |
| Warm query-cache p50 / p95 | 1 / 1 ms |
| Network fetches/query (cold) | 7.2 mean, 8 median, 295 total |
| robots.txt skips (cold) | 0 |
| Block rate (hard/soft) | 0.0% |

By language:

| Language | Judged | Hit@5 | nDCG@5 |
|---|---|---|---|
| English | 27 | **96.3%** (26/27) | 0.821 |
| Turkish | 14 | **85.7%** (12/14) | 0.650 |

### Failures (honest)
- **`rtx 4070 super review` (EN)** — the exact Tom's Hardware review is from
  2024-01; the Tom's adapter only sees the 6 most recent monthly sitemaps, so
  the review is not a candidate. The ranker returns RTX 50-series news and
  RTX 2060/2070 repair guides instead. This is a sitemap-recency coverage gap,
  not a ranking bug.
- **`vaillant kombi arıza` (TR)** — zero candidates. The query is Turkish, the
  available Vaillant source is English (`/service/boiler-fault-codes/`), and
  the Turkish tech sites do not cover boiler faults. No source has the
  content.
- **`samsung buzdolabı sorunu` (TR)** — zero candidates. The available
  ConsumerAffairs Samsung refrigerator page is English; Turkish sources do not
  cover the query.
- **`anker a1257 recall` (unjudged)** — CPSC has Anker A1263/A1642/A1647/A1652
  recalls but no A1257 recall; the API returns zero rows for the model.
  Reported, not scored.

Other quality notes: `best oled tv for movies` had relevant results but a low
nDCG because the top-5 mixed movie articles with TV recommendations;
`honda cr-v review` hit the current-year review but ranked a few older
comparisons above it; `iphone ekran çizgi` and the GUBIS category queries hit
relevant pages but not the specific judged ones at the top.

### What worked
- Model-code matching (`c1`, `a1257`, `note7`) plus brand/product filters
  turned `lg c1 burn in` from generic OLED articles into the exact C1 review.
- Collapsing GSMArena paginated review URLs (`...review-2670p7.php` →
  `...review-2670.php`) removed duplicate pages from the pool.
- The CPSC official API is far better than its stale sitemap: Anker and
  Peloton recalls are found reliably. CPSC site search itself is
  robots-disallowed (`/search?` and `/Recalls?*`).
- GÜBİS RSS is the only practical Turkish recall source; site search is a
  POST form with an anti-forgery token.
- Review/price intent filters stop repair guides from beating reviews and make
  Turkish price queries return price pages.

### Linkup side-by-side (not run)
`eval/linkup_compare.py` is a stub. It does nothing unless `LINKUP_API_KEY` is
set and `--run` is passed; Linkup pricing applies. It reads the same
`eval/queries.jsonl` and reports hit@5 / nDCG@5 so the owner can compare later.
Read https://docs.linkup.so for the current schema.

---

## Layout
```
kriko-magnet/
  README.md            master overview
  RESULTS.md           this file
  fetchlib/            shared polite fetch layer + extractor
  sites/               registry.yaml, planner, adapters, healthcheck
  eval/                judged queries, harness, Linkup stub, latest results
  tests/               pytest: robots, rate limit, adapters, planner
  stage1_py/           Tranco cache, corpus, ranker, benchmark, tuning, parity
  stage1_rs/           Rust port (cargo) — ranker + --scores/--bench modes
  stage2_fetch/        URL roster, status check, fetch+telemetry benchmark
  stage3_pipeline/     cache, worker, pipeline, FastAPI app, cold/warm bench
  data/                Tranco list, corpus, queries, runtime caches (ignored)
```
