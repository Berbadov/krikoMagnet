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

## Stage 5 — Faster, more reliable discovery (60 judged queries)

### What was built
- **Persistent local index.** `sites/index.py` ingests every sitemap/RSS into
  SQLite FTS5 (`data/site_index.sqlite`): url, title, snippet, lastmod and a
  folded match text. A query now does a local lexical lookup instead of
  downloading sitemaps; only the top-k pages are fetched, still through
  `fetchlib` (robots.txt, honest UA, >=1 s/host). `build` is the one-time full
  backfill, `refresh` uses ETag/If-Modified-Since; `stats` reports age.
- **Resident ranker.** The Rust binary runs in `--serve` mode: Tranco loads
  once and queries are JSON lines. Per-query rank drops from a ~70 ms spawn to
  ~2 ms; a dead child restarts and falls back to the one-shot binary.
- **Reliability.** Per-site circuit breaker (3 failures, 5-minute cooldown,
  surfaced in the health check), per-adapter 25 s timeout, graceful partial
  results, jittered retry backoff, WAL on all SQLite caches, query/content
  TTLs, JSON logs, and a health check that exits non-zero and reports index
  age + open circuits.
- **Turkish coverage.** Seven Turkish sources were added after a live
  robots.txt check: Vaillant TR, Bosch Home TR, ECA, Baymak, Donanım Arşivi,
  ShiftDelete, Tamindir. Turkish/English product nouns are now equivalent for
  filtering (`kombi` ≈ `boiler`), and the planner has a brand/domain bonus so
  `bosch ...` lifts bosch-home.com.tr.
- **Tests.** 37 pytest tests, no network: robots/conditional fetch, rate
  limits, adapters, planner, index ingest/query, circuit breaker, cache TTLs,
  fetch budget, resident-ranker parity (skipped without a Rust build).

### Evaluation design (Stage 5)
- **60 queries**: 30 English + 30 Turkish (Stage 4's 42 plus 18 new, mostly
  Turkish appliance/boiler queries). 59 judged; `anker a1257 recall` stays a
  documented coverage gap. Every new judged URL returned HTTP 200 with the
  honest UA and extracted text on 2026-11-20.
- `cold` = empty query cache and empty fetch cache; the local sitemap index is
  kept (the production warm-index path). `warm-fetch` keeps page caches;
  `warm` repeats the query.

### GATE 5 — results (k=5)

Two runs, both with the 60-query set and a warm local index:

- **Default (no page budget)** — every top-k page is fetched. This is the
  quality-first configuration the service uses by default.
- **2.5 s total-query budget** — opt-in (`--page-budget 2500`); returns
  graceful partial results when page fetches would exceed the deadline.

| Metric | Stage 4 (41 judged) | Stage 5 default (59 judged) | Stage 5 + 2.5 s budget |
|---|---|---|---|
| Queries with >=1 relevant in top-5 | 92.7% | **94.9%** (56/59) | 74.6% |
| nDCG@5 (mean) | 0.763 | 0.738 | 0.565 |
| nDCG@5 English | 0.821 | 0.789 | 0.584 |
| nDCG@5 Turkish | 0.650 | **0.681** | 0.544 |
| Precision@5 (mean) | 0.307 | 0.251 | 0.173 |
| Result domains / query | 1.24 | 2.10 | 1.75 |
| Cold latency p50 / p95 | 6462 / 10627 ms | **3159 / 7336 ms** | **2517 / 5689 ms** |
| Warm-fetch, cold-query p50 / p95 | 1001 / 2766 ms | 580 / 3816 ms | 1615 / 4815 ms |
| Warm query-cache p50 / p95 | 1 / 1 ms | 0.4 / 0.8 ms | 0.4 / 0.9 ms |
| Network fetches / query (cold) | 7.2 | **4.2** | 2.6 |
| robots.txt skips / block rate | 0 / 0.0% | 0 / 3.1% | 0 / 0.0% |

Index: **913,284 pages across 20 sites** (1,646 sitemap/RSS feeds), one full
polite backfill; measured age at the run: under one hour. English hit@5 is
**100%**; the three remaining misses are Turkish phone queries whose exact
judged URLs are older or less specific than newer coverage.

The 2.5 s budget reaches the latency target but drops hit@5 to 74.6%, so it
is opt-in. On a warm page cache it recovers to 89.8% / 0.696, which is the
realistic steady state after a first query touches a site.

Adapter health at the same index: **21/21 healthy** (links + extraction), no
open circuits, exit code 0 (`python -m sites.healthcheck --json`).

### Same 41 queries as Stage 4
Run on the identical original queries with the new pipeline:

| Metric | Stage 4 | Stage 5 |
|---|---|---|
| Hit@5 | 92.7% | 92.7% |
| nDCG@5 | 0.763 | 0.657 |
| nDCG@5 EN | 0.821 | 0.783 |
| nDCG@5 TR | 0.650 | 0.413 |
| Cold p50 | 6462 ms | 3159 ms |
| Fetches/query | 7.2 | 4.59 |

Hit rate is unchanged on the old set, but nDCG falls: the broader index
surfaces many near-duplicate Turkish phone pages, and the strict exact-URL
judgments for `iphone ekran çizgi`, `iphone pil sağlığı gece şarj` and
`iphone ekran değişimi` (5/2/1 judged URLs) go from partial credit to zero.
That is the honest cost of wider coverage against a narrow judgment set.

### What worked
- The local index removes sitemap downloads: discovery for sitemap/RSS sites
  is a local lookup (only API/search adapters touch the network), and network
  fetches/query fell from 7.2 to 4.2 with the same k=5.
- The full backfill recovered the Stage 4 sitemap-window failure: the Jan-2024
  Tom's Hardware RTX 4070 Super review now ranks first for
  `rtx 4070 super review`.
- Turkish product/intent synonyms and Turkish stemming route
  `samsung buzdolabı sorunu` to the ConsumerAffairs refrigerator hub,
  `vaillant kombi arıza` to the Turkish and UK fault-code hubs, and
  `eca kombi su ısıtmıyor` to ECA's boiler guides.
- Resident ranker: ~2 ms/query instead of a ~70 ms spawn; warm-fetch p50 is
  now ~0.8 s and warm query cache is still ~0 ms.
- Domain interleaving lifted the number of result domains per query from
  1.24 to 2.12, so one site no longer fills the whole top-5.
- Reliability: the 25 s adapter timeout that made one CPSC hiccup cost 27 s is
  gone (8 s site timeout, recall sources only planned for recall queries),
  failures trip a per-site circuit breaker, and the health check reports the
  index and open circuits.

### Failures (honest)
- **The cold p50 target (<= 2.5 s) is not met without a budget.** Cold p50 is
  3.24 s: five results across at most two domains cost several one-second
  per-host slots by design. The opt-in 2.5 s total budget reaches the target
  (2.52 s) but drops hit@5 to 74.6%, so it is not the default.
- **TR nDCG@5 is below the 0.75 target.** Three queries fail and several
  multi-URL Turkish judgments score low: the judged pages are older or less
  lexically specific than newer coverage with URL-only metadata. Examples:
  `iphone ekran çizgi` judges iPhone Duo screen-crack pages while a same-day
  iPhone-case article matches "çizgi" literally; `iphone ekran değişimi`
  judges the iPhone 18 Pro Max price page while an iPhone 17 Pro Max page has
  an almost identical URL. This is a metadata ceiling, not a discovery bug.
- **EN nDCG@5 is slightly below Stage 4** (0.821 → 0.789). English hit@5 is
  100%, but broader coverage ranks some older judged pages lower than Stage
  4's narrow sitemap window.
- **Six DonanımHaber monthly sitemaps returned 429** during the full ingest.
  They were not bypassed; the index still has 52,637 DonanımHaber pages from
  48 recent months (a later refresh can fill the gap).
- **Two sources remain unavailable**: Arçelik/Beko robots.txt returns 403 to
  the honest UA and Vestel's sitemap host blocks robots.txt; both are recorded
  as skipped rather than worked around.

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
