# Kriko Magnet — Search / Crawl / Extraction Prototype

**Status:** v0.1.0 — all 3 staged gates PASS.
**Operational name:** Kriko Magnet (experimental).
**Goal:** prove a zero-cost, standalone, open-source search & extraction stack:
free-signal hybrid ranking, direct site fetching with block telemetry, and a
minimal full-stack pipeline.
**Location:** all code, data, and docs live in the `kriko-magnet/` folder
(this repo).

This document is the master overview. Per-component details live in each
stage's `README.md`.

---

## Architecture

```
query
  |
  v
[STAGE 3 pipeline]
  |-- query-level cache (SQLite)
  |
  v
[STAGE 1 ranker]  Rust: BM25 + Tranco prior + freshness
  |  top-K candidate URLs
  v
[STAGE 2 fetch worker]  curl_cffi + trafilatura (selectolax fallback)
  |  clean markdown
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
| 1B | `stage1_rs/` | Rust port; numerical parity with Python; p95 <= 2 ms |
| 2 | `stage2_fetch/` | Real-world direct fetching + block-rate telemetry |
| 3 | `stage3_pipeline/` | End-to-end pipeline + FastAPI + cache benchmark |

### Data
- `data/tranco_top100k.txt` — Tranco Top-1M daily list (top 100k slice), cached.
- `data/corpus.jsonl`, `data/queries.json` — Stage-1 synthetic 500-doc test corpus + 15 queries.
- `data/pipeline_cache.sqlite` — Stage-3 SQLite cache (content + query-level).

---

## Results at a glance (all gates green)

| Gate | Metric | Result | Threshold | Verdict |
|---|---|---|---|---|
| 1A | NDCG@5 (mean) | **0.910** | >= 0.80 | PASS |
| 1A | Top-5 precision (mean) | 0.707 | — | — |
| 1B | Rust rank-correlation vs Python (15/15) | identical | = 1.0 | PASS |
| 1B | Rust latency p95 | **0.54 ms** | <= 2 ms | PASS |
| 2 | Clean extraction success | **80.0%** (32/40) | >= 75% | PASS |
| 2 | Hard/soft block rate | **17.5%** (7/40) | <= 25% | PASS |
| 3 | Warm-cache latency (10 queries) | **0.29 ms** median / 0.42 ms p95 | < 10 ms | PASS |

Full tables: see `RESULTS.md`.

---

## Key engineering decisions & findings

1. **Tranco download behind Cloudflare:** `tranco-list.eu` returned Cloudflare
   525 (SSL handshake) to plain HTTP clients. Fixed by fetching the **S3
   origin** (`tranco-list.s3.amazonaws.com`) with `curl_cffi` TLS
   impersonation (`impersonate="chrome"`).
2. **Additive hybrid needs a lexical gate.** A pure additive
   `alpha*BM25 + beta*prior + gamma*fresh` let zero-match but high-authority pages
   (e.g. apple.com) float into top-5 on priors alone. Fix: only docs with
   >=1 query-token match are ranking candidates; the prior/freshness then
   rescale real matches. Weights tuned (`alpha=0.40, beta=0.60, gamma=0.15`).
3. **Spam is lexical-perfect, so authority must be additive, not
   multiplicative.** A multiplicative `BM25*(1+beta*prior+gamma*fresh)` lost to SEO
   spam because spam's full-query title gives it BM25≈1 and the multiplier
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

---

## How to run

Prereqs: Python 3.14 (with `requests`, `curl_cffi`, `trafilatura`,
`selectolax`, `fastapi`, `uvicorn`, `pytest`), Rust 1.96+.

```bash
# Stage 1A — Python prototype benchmark (prints metrics table)
python stage1_py/benchmark.py

# Stage 1B — Rust build + parity + latency
cargo build --release --manifest-path stage1_rs/Cargo.toml
python stage1_py/parity_rs.py     # parity + Rust p50/p95

# Stage 2 — fetch + block telemetry (40 real URLs)
python stage2_fetch/fetch_bench.py

# Stage 3 — end-to-end cold/warm benchmark
python stage3_pipeline/bench.py

# Stage 3 — FastAPI service (POST /search)
python stage3_pipeline/app.py       # http://127.0.0.1:8000
curl -X POST localhost:8000/search -H 'Content-Type: application/json' \
     -d '{"query":"sqlite window functions tutorial","k":3}'
```

Other helpers: `stage1_py/tune.py` (weight search), `stage1_py/diag.py`
(per-query score breakdown), `stage1_py/corpus.py` (regenerate corpus),
`stage3_pipeline/gen_candidates.py` (regenerate pipeline candidates).

---

## Blocked domains (Stage 2, honestly reported)
- **Hard:** `serverfault.com` → 403 (Cloudflare).
- **Soft** (JS-shell / challenge, <300 clean chars):
  `support.hp.com` (0), `quora.com` (118), `reddit.com` x4 (144).
- **HTTP error (non-block):** `arstechnica.com` → 405.

---

## Limitations / next steps
- Ranking is a hand-tuned additive hybrid on a synthetic corpus; real
  learning-to-rank or grid/auto-tuning is future work.
- Reddit/Quora/HP yield no static content (JS-rendered) — would require
  `old.reddit.com`, an authenticated session, or a headless renderer (out of
  scope: the mandate forbade headless browsers in Stage 2).
- For pipeline is a minimal demo: candidate pool is curated (26 URLs), ranking
  swall per-query Rust subprocess (fine for demo; a resident daemon or
  PyO3 bindings would remove spawn overhead).
- Cache invalidation (TTL) is not implemented.
