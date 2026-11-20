# Kriko Magnet — Search / Crawl / Extraction Prototype

**Status:** v0.2.0 — Stages 1–3 gates pass; Stage 4 evaluation is in.
**Operational name:** Kriko Magnet (experimental).
**Goal:** prove a zero-cost, standalone search & extraction stack for Kriko
(“check before you buy”) with **no search-engine provider**: no Google, Bing,
SearXNG or Linkup. Site importance comes from the Tranco ranking; pages are
found through each site’s own search/sitemap/RSS/API, then fetched directly.
**Location:** all code, data, and docs live in the `kriko-magnet/` folder
(this repo).

This document is the master overview. Per-component details live in each
stage’s `README.md`; the site registry and adapters are documented in
`sites/README.md`, the shared fetch layer in `fetchlib/README.md`, and the
evaluation in `eval/README.md`.

---

## Architecture

```
query
  |
  v
[STAGE 3 pipeline]  query-level cache (SQLite, TTL)
  |-- query planner: topics + language (bilingual keyword lexicon, no LLM)
  |-- site registry: 21 high-value sites, each with a verified method
  |-- local index (SQLite FTS5): sitemap/RSS url+title+lastmod metadata
  |     (live site search / official API still run as adapters)
  |
  v
candidates (url, title, snippet, date, discovery score)
  |
  v
[STAGE 1 ranker]  Rust: BM25 + Tranco prior + freshness + discovery prior
  |  resident --serve process; top-K
  v
[STAGE 2 fetch worker]  polite shared fetch: robots.txt, honest UA,
  |                     per-host spacing, 429 backoff, TTL cache
  |                     + trafilatura (selectolax fallback)
  v
[SQLite cache]  url_hash + query_hash
  |
  v
structured JSON results
```

### Stages

| Stage | Directory | What it proves |
|---|---|---|
| 1A | `stage1_py/` | Python prototype: BM25 + Tranco authority + freshness; NDCG@5 |
| 1B | `stage1_rs/` | Rust port; numerical parity with Python; p95 <= 2 ms; `--serve` resident mode |
| 2 | `stage2_fetch/` | Real-world direct fetching + block-rate telemetry |
| 3 | `stage3_pipeline/` | End-to-end pipeline + FastAPI + cache benchmark |
| 4 | `sites/`, `eval/` | Query -> pages without a search engine; honest judged eval |
| 5 | `sites/index.py`, `eval/` | Persistent sitemap index, resident ranker, Turkish sources, 60-query eval |

### Data
- `data/tranco_top100k.txt` — Tranco Top-1M list (top 100k slice), cached; re-downloadable.
- `data/corpus.jsonl`, `data/queries.json` — Stage-1 synthetic 500-doc corpus + 15 queries.
- `data/pipeline_cache.sqlite`, `data/fetch_cache.sqlite` — runtime caches (ignored).
- `data/eval_cache.sqlite`, `data/eval_fetch.sqlite` — evaluation caches (ignored).
- `eval/queries.jsonl` — 42 realistic judged/coverage-gap queries.

---

## Results at a glance

| Gate | Metric | Result | Threshold | Verdict |
|---|---|---|---|---|
| 1A | NDCG@5 (mean) | **0.910** | >= 0.80 | PASS |
| 1A | Top-5 precision (mean) | 0.707 | — | — |
| 1B | Rust rank-correlation vs Python (15/15) | identical | = 1.0 | PASS |
| 1B | Rust latency p95 | **0.54 ms** | <= 2 ms | PASS |
| 2 | Clean extraction success | **81.8%** (27/33 fetchable) | >= 75% | PASS |
| 2 | Hard/soft block rate | **15.2%** (5/33 fetchable) | <= 25% | PASS |
| 2 | robots.txt skips | 7 (reported, not fetched) | — | — |
| 3 | Registry-mode cold / warm median | **8904 ms / 0.65 ms** | — | — |
| 3 | Registry-mode warm p95 | **0.92 ms** | < 10 ms | PASS |
| 4 | Queries with >=1 relevant in top-5 | **92.7%** (38/41) | — | — |
| 4 | nDCG@5 (mean) | **0.763** | — | — |
| 4 | Cold / warm-fetch p50 latency | **6462 / 1001 ms** | — | — |

Full tables and failure notes: see `RESULTS.md`.

---

## Key engineering decisions & findings

1. **Tranco 525 was an SSL handshake issue, not a bot wall.**
   `tranco-list.eu` returned 525 to plain HTTP clients; the **S3 origin**
   (`tranco-list.s3.amazonaws.com`) works with an honest user agent. No TLS
   impersonation is used.
2. **Additive hybrid needs a lexical gate.** A pure additive
   `alpha*BM25 + beta*prior + gamma*fresh` let zero-match but high-authority
   pages (e.g. apple.com) float into top-5 on priors alone. Fix: only docs with
   >=1 query-token match are ranking candidates; the prior/freshness then
   rescale real matches. Weights tuned (`alpha=0.40, beta=0.60, gamma=0.15`).
3. **Spam is lexical-perfect, so authority must be additive, not
   multiplicative.** A multiplicative `BM25*(1+beta*prior+gamma*fresh)` lost to
   SEO spam because spam’s full-query title gives it BM25≈1 and the multiplier
   amplifies that advantage faster than the prior compensates. The gated
   additive form is what wins.
4. **Rust speed:** the first port re-tokenized the whole corpus on every rank
   call (~12 ms p95). Precomputing token arrays + inverted stats at load and
   reusing a frequency buffer dropped p95 to **0.54 ms**.
5. **Block-marker false positives:** matching challenge words ("captcha",
   "access denied", "just a moment") against raw HTML flagged successful pages
   (apnews: 4.9k clean chars) as soft-blocks. Markers are now matched only
   against the **extracted text**, i.e. the actual challenge/render outcome.
6. **Warm-cache <10 ms needed query-level caching.** Even with per-URL cache
   hits, the per-query Rust subprocess spawn (~70 ms) broke the <10 ms target.
   A query-level SQLite result cache lets repeated lookups return without
   re-ranking or re-fetching (0.2–0.6 ms).
7. **Polite fetching is a first-class layer, not an afterthought.**
   `fetchlib` checks and caches robots.txt, identifies as
   `KrikoMagnet/0.2 (+https://kriko.cc/bot)`, spaces requests per host (>=1 s,
   Crawl-delay honoured), caps global concurrency, respects 429/Retry-After,
   and caches responses with a TTL. Chrome TLS impersonation is an explicit
   opt-in flag, never a default (see `fetchlib/README.md`).
8. **The missing link was site-level discovery.** A hand-curated 26-URL list
   cannot answer arbitrary queries. The registry maps topics -> sites and runs
   each site’s own method (search page, sitemap/RSS, official API), then merges
   candidates. Topic relevance and exact query terms outrank Tranco popularity.
9. **Heuristic filters matter on real pages.** Pure BM25 on URL slugs lets repair
   guides beat reviews, paginated GSMArena pages crowd out the article, and
   “Note7” fail to match “Note 7”. Small deterministic filters (model codes,
   brands, product nouns, review/price intent, GSMArena pagination collapse)
   plus a discovery prior made the real-web eval usable.
10. **Stage 1 parity is unchanged.** The optional `discovery` field in the Rust
    corpus defaults to 0, so the 15-query Python/Rust parity and latency gates
    still hold. Stage 3/4 set it to the adapter match score.

---

## How to run

Prereqs: Python 3.14 with `curl_cffi`, `trafilatura`, `selectolax`,
`protego`, `PyYAML`, `fastapi`, `uvicorn`, `pytest`; Rust 1.96+.
Install with `python -m pip install -r requirements.txt`.

```bash
# Stage 1A — Python prototype benchmark
python stage1_py/benchmark.py

# Stage 1B — Rust build + parity + latency
cargo build --release --manifest-path stage1_rs/Cargo.toml
python stage1_py/parity_rs.py

# Stage 2 — fetch + block telemetry (40 real URLs, robots-aware)
python stage2_fetch/fetch_bench.py

# Stage 3 — registry-mode cold/warm benchmark (8 Kriko queries)
python stage3_pipeline/bench.py --fresh
# old hand-curated fallback for comparison:
python stage3_pipeline/bench.py --curated --fresh

# FastAPI service
python stage3_pipeline/app.py       # http://127.0.0.1:8000
curl -X POST localhost:8000/search -H 'Content-Type: application/json' \
     -d '{"query":"lg c1 burn in","k":3}'

# Adapter health check (one known query per site; non-zero exit on a break)
python -m sites.healthcheck
python -m sites.healthcheck --json > eval/health_latest.json

# Local sitemap index (one-time polite backfill, then conditional refresh)
python -m sites.index build
python -m sites.index refresh
python -m sites.index stats

# Honest evaluation (60 judged queries + 1 coverage gap)
python eval/run_eval.py --fresh --json eval/results_latest.json \
  --markdown eval/results_latest.md

# Tests (robots compliance, rate limiting, adapters, planner)
python -m pytest
```

Other helpers: `stage1_py/tune.py`, `stage1_py/diag.py`,
`stage1_py/corpus.py`, `stage3_pipeline/gen_candidates.py` (regenerate the
curated fallback), `eval/linkup_compare.py` (owner-only Linkup stub; not run by
the prototype).

---

## Site registry at a glance

14 active sites: GSMArena, RTINGS, DonanımHaber, CPSC (official API), iFixit,
Vaillant UK, ConsumerAffairs, What Hi-Fi?, MacRumors, Tom’s Hardware,
Car and Driver, Webtekno, Chip Online, and GÜBİS (Turkish unsafe-product RSS).
Method and robots notes per site are in `sites/README.md`.

Skipped (with reasons): `sikayetvar.com` (429/CAPTCHA), `technopat.net`,
`sahibinden.com`, `hepsiburada.com`, `nhtsa.gov` (403), `trendyol.com`
(search disallowed, sitemap 403), `old.reddit.com` (`Disallow: /`),
`techpowerup.com` (robots forbids automated data mining/LLM use),
`partselect.com` (product pages 403), Safety Gate (JS-only SPA), `arabam.com`
(search disallowed; 10 MB classified sitemaps), and several Cloudflare-blocked
sites. Full list with reasons in `sites/registry.yaml`.

---

## Limitations / next steps

- **Sitemap recency windows miss older evergreen reviews.** Tom’s Hardware’s
  RTX 4070 Super review (Jan 2024) is outside the 6-month sitemap slice used
  for a recent query. Next step: keep a persistent sitemap index or add a
  URL-pattern/backfill method for category review pages.
- **Turkish coverage is thin for cars/appliances.** `sikayetvar.com` and
  `technopat.net` block honest requests; `arabam.com` only exposes huge
  classified sitemaps. Turkish car-maintenance and appliance-repair queries
  therefore return zero candidates.
- **The registry is hand-maintained.** Selectors and sitemap paths can break;
  `python -m sites.healthcheck` is the early warning. It is 14 sites, not the
  web.
- **No headless browser and no CAPTCHA solving.** Sites that return a JS shell
  or challenge are skipped, not bypassed.
- **Ranking is still a hand-tuned hybrid.** The discovery prior and the
  model/brand/product/intent filters are deterministic heuristics; a learned
  ranker remains future work.
- **Eval judgments are exact-URL and strict.** Near-miss pages count as misses,
  so nDCG is conservative.
- **Linkup was not called.** `eval/linkup_compare.py` is a stub for the owner
  to run a side-by-side later with their own key; it costs money.
