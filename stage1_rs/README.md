# Stage 1B — Rust Port of the Hybrid Ranker

Ports the Stage 1A Python scorer to Rust to prove sub-millisecond ranking with
bit-for-bit numerical parity.

## Build / run
```bash
cargo build --release
# rank the 15 benchmark queries (prints JSON: query -> ranked doc ids)
./target/release/kriko-magnet-ranker \
  --corpus ../data/corpus.jsonl --queries ../data/queries.json \
  --tranco ../data/tranco_top100k.txt --now "$(date +%s)"

# scores mode (used by Stage 3 pipeline)
./target/release/kriko-magnet-ranker ... --scores

# latency benchmark
./target/release/kriko-magnet-ranker ... --bench
```

CLI flags: `--corpus <jsonl> --queries <json> --tranco <txt> --now <epoch>
--bench --scores`.

## Stage 3/4 discovery prior
Corpus records may carry an optional `discovery` field (0..1). It is added to
the final score with `delta=0.30`; old corpora omit it (default 0), so Stage 1
parity and latency are unchanged.

## Design
- **Tokenization** mirrors Python's `[a-z0-9]+` over lowercased text.
- **Corpus stats precomputed at load:** token arrays, doc lengths, term doc
  frequency, avg doc length. No re-tokenization at query time.
- **Query terms** are sorted + deduped (mirrors Python's `set(qterms)`).
- **Scoring** reuses a frequency buffer of size `nquery`; only query-term
  hits are counted in a single pass over each doc's precomputed tokens.
- **Gate + additive blend** identical to Python (α=0.40, β=0.60, γ=0.15).
- **Stable sort** by `(score desc, index asc)` to match Python's stable sort.

## Gate 1B result (PASS)
| Metric | Value | Gate |
|---|---|---|
| Full-rank parity vs Python (15/15 queries) | identical | correlation = 1.0 ✅ |
| Top-5 parity | identical | 1.0 ✅ |
| Latency p50 / p95 | **0.45 / 0.54 ms** | p95 ≤ 2 ms ✅ |

## Why it's fast
First port re-tokenized the whole corpus per query (~12 ms p95). Moving
tokenization/inverted stats to load time and reusing a frequency buffer cut
p95 to **0.54 ms** — ~22× faster.
