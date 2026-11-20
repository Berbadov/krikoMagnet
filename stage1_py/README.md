# Stage 1A — Hybrid Ranker (Python prototype)

Proves zero-cost candidate ranking: **BM25(title+snippet) + log-scaled Tranco
domain-authority prior + exponential freshness**, with a **lexical gate** so
priors never surface zero-match pages.

## Files
| File | Purpose |
|---|---|
| `tranco_cache.py` | Download/cache Tranco top-1M from the S3 origin with an honest user agent (`tranco-list.eu`'s 525 was an SSL handshake issue; no impersonation needed); exposes `load_top100k()` |
| `corpus.py` | Regenerates the 500-doc corpus + 15 judged queries (60 relevant, 45 SEO-spam, 395 filler). Writes `data/corpus.jsonl`, `data/queries.json`. Prints authority-presence diagnostics |
| `ranker.py` | `Ranker` class: BM25 (k1=1.2, b=0.75), prior `ln(1e6/rank)`, freshness `exp(-days/90)`, weight blend. `now` can be injected for parity |
| `benchmark.py` | Prints the metrics table: P@5, NDCG@5, p50/p95 latency (per query) |
| `tune.py` | Grid-search over `α/β/γ` weights |
| `diag.py` | Dump top-N with per-signal breakdown (`bm25s`, `priors`, `fresh`) for one query |
| `parity_rs.py` | Runs Python + Rust with identical epoch; asserts full-rank identity; runs Rust latency bench (see Stage 1B) |

## Scoring formula
1. Candidate docs = those with ≥1 query-token match (lexical gate).
2. `bm25_raw` over `title + snippet`; `bm25_s = bm25_raw / max(bm25_raw)` over candidates.
3. `prior = ln(1e6 / max(rank,1))` if domain in Tranco top-100k, else 0; `prior_s = prior / ln(1e6)`.
4. `fresh = exp(-max(days,0)/90)`.
5. `score = 0.40·bm25_s + 0.60·prior_s + 0.15·fresh` (tuned).

## Gate 1A result (PASS)
| Metric | Value | Gate |
|---|---|---|
| NDCG@5 (mean) | **0.910** | ≥ 0.80 ✅ |
| Top-5 Precision (mean) | 0.707 | — |
| Latency p50 / p95 | 20.8 / 33.1 ms | (speed gate at 1B) |

## Run
```bash
python corpus.py          # regenerate corpus (optional)
python benchmark.py       # metrics table
python tune.py            # weight search (optional)
python diag.py q6         # inspect one query's ranking
python parity_rs.py       # Stage 1B parity + Rust bench
```
