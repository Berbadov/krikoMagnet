# Kriko Magnet

Engine-free, polite search for Kriko: given a query across consumer, technical,
or research domains, it finds and extracts pages directly from trusted sites
without calling Google, Bing, SearXNG, or any search API.

Rewritten as a single minimal Rust binary (`magnet`) with Bun TypeScript tooling
(`eval/eval.ts`).

## Architecture

1. **Plan & Discover** ([src/planner.rs](src/planner.rs), [src/index.rs](src/index.rs), [src/discover.rs](src/discover.rs)) —
   index-first domain-agnostic retrieval queries all indexed golden sources in
   `data/site_index.sqlite` weighted by language affinity and Bayesian site
   trust (`data/sources.sqlite`); live adapters and 5 general reference endpoints
   run concurrently or on fallback; Common Crawl CDX runs only when empty.
2. **Scout & Source Estimation** ([src/measure.rs](src/measure.rs), [src/scout.rs](src/scout.rs)) —
   empirical structural & linguistic shape estimator calibrates golden baselines
   (`data/source_baseline.json`) and autonomously vets candidate domains politely
   with sequential Bayesian stopping (`magnet scout ... --promote --index`).
3. **Rank** ([src/ranker.rs](src/ranker.rs)) — in-process BM25 + Tranco
   authority prior + exponential freshness + discovery score.
4. **Fetch & Extract** ([src/http.rs](src/http.rs), [src/extract.rs](src/extract.rs)) —
   honest `KrikoMagnet/0.2` user agent, `robots.txt` enforced and cached, >=1 s
   per host + `Crawl-delay`, conditional GET, `Retry-After` backoff, pure-Rust
   Readability + SSR/JSON-LD prose extraction, and SQLite WAL caches.
5. **Serve & MCP** ([src/main.rs](src/main.rs)) — CLI, Axum HTTP server
   (`POST /search`, `GET /health`), and stdio MCP server (`magnet_search`,
   `magnet_fetch`, `magnet_health`).

## Quick Start

Requires Rust 1.85+ and Bun 1.x.

```bash
cargo build --release
cargo test

# One-time polite sitemap/RSS index backfill (or reuse existing data/site_index.sqlite)
./target/release/magnet index build
./target/release/magnet index stats

# CLI search, fetch, health, calibration, and autonomous source scouting
./target/release/magnet search "lg c1 burn in" -k 5
./target/release/magnet health
./target/release/magnet calibrate
./target/release/magnet measure https://example.com/a https://example.com/b
./target/release/magnet scout docs.rs --sample 6

# HTTP server (http://127.0.0.1:8000)
./target/release/magnet serve

# Stdio MCP server (for OpenCode / MCP clients)
./target/release/magnet mcp

# Evaluation, full subsystem suite, & paid-provider comparison via Bun
bun run eval:fresh
bun run bench:pro
bun run bench -- --dry-run
```

## Benchmarks

Measured on branch `rewrite/rust` (`magnet v0.3.0`, release build) across the
60-query EN/TR evaluation suite ([eval/queries.jsonl](eval/queries.jsonl), 59
judged) using honest `KrikoMagnet/0.2` HTTP requests with `robots.txt`
enforcement and `>=1 s` per-host rate limiting (`0` robots violations, `2.3%`
soft/hard block rate, `$0.0000` search API cost).

### End-to-End IR, Latency & Extraction (`59` Judged Queries, Top-5)

Run via `bun run eval/eval.ts --fresh --pro` (`budget=0ms`, cold HTTP fetches ->
warm fetch cache -> warm query cache):

| Metric | Result |
|---|---|
| Judged queries | `59` (`31` EN / `28` TR) |
| **Hit@1 / Hit@3 / Hit@5** | `62.7%` / `86.4%` / `93.2% [86.4–98.3%]` |
| **MRR@5 (95% CI)** | `0.745 [0.655–0.832]` |
| **nDCG@1 / nDCG@3 / nDCG@5 (95% CI)** | `0.627` / `0.678` / `0.730 [0.650–0.807]` |
| **Precision@5 / Domain-Hit@5** | `0.264` / `98.3%` |
| Cold latency (`p50 / p90 / p95 / p99`) | `5905 / 12878 / 16945 / 18200 ms` (`0.15 QPS`) |
| Warm-fetch latency (`p50 / p95 / p99`) | `333 / 4435 / 6291 ms` (`0.9 QPS`) |
| Warm query-cache (`p50 / p95 / p99`) | `1.5 / 3.9 / 6.3 ms` (`545 QPS`) |
| Extraction (`median chars / <300 short rate`) | `5258 chars` / `0.0%` |
| Language slice (`EN` / `TR` Hit@5 · nDCG@5) | EN `100.0% · 0.750` / TR `85.7% · 0.709` |

### Head-to-Head Provider Benchmark (`--providers all --limit 50`, `budget=15s`)

Run live on the first `50` queries (`49` judged; `$0.34` total API spend for
DeepSeek + Linkup):

| Provider | Hit@1 | Hit@5 (95% CI) | MRR@5 | nDCG@5 (95% CI) | Domain-Hit@5 | p50 / p95 (ms) | QPS | Cost (`/1k`) |
|---|---:|---|---|---:|---:|---|---:|---:|
| **`magnet` (cold, 15s budget)** | `55.1%` | `83.7% [73.5–93.9%]` | `0.657` | `0.640 [0.533–0.729]` | `91.8%` | `7930 / 15059` | `0.1` | `$0.0000` (`$0.00`) |
| **`magnet_warm_fetch`** | `61.2%` | `91.8% [83.7–98.0%]` | `0.723` | `0.698 [0.603–0.783]` | `98.0%` | `457 / 12052` | `0.4` | `$0.0000` (`$0.00`) |
| **`magnet_warm_cache`** | `61.2%` | `91.8% [83.7–98.0%]` | `0.723` | `0.698 [0.603–0.783]` | `98.0%` | `1 / 10` | `459.9` | `$0.0000` (`$0.00`) |
| **`deepseek`** | `6.1%` | `6.1% [0.0–14.3%]` | `0.061` | `0.053 [0.000–0.119]` | `16.3%` | `664 / 1015` | `1.4` | `$0.0375` (`$0.75`) |
| **`linkup`** | `4.1%` | `12.2% [4.1–20.4%]` | `0.069` | `0.068 [0.018–0.127]` | `16.3%` | `1632 / 2115` | `0.6` | `$0.3000` (`$6.00`) |

- **Paired Win / Tie / Loss (`49` judged):** `magnet` vs `deepseek` = `39 / 9 / 1` (`Hit@5` & `nDCG@5`); `magnet` vs `linkup` = `37 / 10 / 2` (`Hit@5`), `40 / 7 / 2` (`nDCG@5`).
- **Important methodology caveat (closed ground-truth domain bias):** [eval/queries.jsonl](eval/queries.jsonl) lists relevant URLs exclusively from Kriko Magnet's `21` curated registry domains. Only `8.6%` of DeepSeek's top-5 URLs and `4.5%` of Linkup's top-5 URLs came from those judged domains (`Domain-Hit@5 = 16.3%` for both, vs `98.0%` for Magnet warm-fetch), so valid off-registry results returned by open-web search (e.g., `techpowerup.com`, `anker.com`, `support.apple.com`, `reddit.com`) are scored as `0`. Conditional on hitting a judged domain, Linkup's `Hit@5` is `75.0%` (`6/8`) and DeepSeek's is `37.5%` (`3/8`); conversely, uncurated open-web results also include off-locale noise (`13.1%` non-target TLD/path rate for DeepSeek such as `.no`/`.de`/`.fr`/`.dk`/`.it` on EN queries, plus `weibo.com` and `.id` pages for Linkup on TR queries) and low-trust SEO/AI-review sites (`historytools.org`, `nvidiareview.com`, `techfuelhq.com`, `review-rating.com`).
- **Budget caveat:** Under a strict `15 s` cold timeout in `fetch_many`, aborted page fetches omit those candidates from `results` (`83.7%` cold vs `91.8%` once fetches are cached in `magnet_warm_fetch`).

### Subsystems, Calibration & Cross-Domain General Queries

- **Footprint:** `10.52 MB` release binary, `920.18 MB` local FTS5 index (`913,284` pages across `20` sites), `12 KB` Beta source-trust DB (`data/sources.sqlite`).
- **Discovery + BM25 Rank (`fetch=false`, `60` queries incl. live JSON APIs):** `p50=222.2 ms`, `p95=3731.6 ms`, `1.1 QPS`.
- **Source Calibration (`magnet calibrate`):** calibrated `23` hosts / `167` cached pages (`6` promoted including `www.rtings.com` at `0.908`, `16` uncertain, `1` reject `www.donanimhaber.com` at `0.137` with `35.6%` affiliate link ratio, median `p_golden=0.845`).
- **Cross-Domain General Probe (`$0.00`):** Wikipedia EN + Crossref + Stack Overflow + Math Stack Exchange + Tom's Hardware returned extracted pages (`3,301–70,118` median chars) across complexity theory, Rust `Pin`/`Unpin`, Bayesian priors, state-space models, genetics, and statistical mechanics.
- **Autonomous Scout Live (`magnet scout --sample 5`):** `docs.rs` (`sitemap`, `n=5`, `p_golden=0.62`, `[0.38, 0.81]`, `uncertain`), `simonwillison.net` (`sitemap`, `n=5`, `p_golden=0.379`, `[0.18, 0.62]`, `uncertain`), `reddit.com` (`none`, `p_golden=0.01`, `reject` via `robots.txt disallows root`).

## Layout

```text
Cargo.toml          single Rust binary crate (`magnet`)
package.json        Bun task scripts
registry.yaml       21 consumer sites + 5 general sources + skipped reasons
src/
  main.rs           CLI, HTTP server (Axum), stdio MCP server
  text.rs           TR/EN folding, stemming, aliases, query normalization
  planner.rs        registry + Tranco loading, topic/site routing
  http.rs           polite HTTP fetcher, robots.txt, rate limiter, SQLite cache
  index.rs          SQLite FTS5 index (build, refresh, stats, query)
  discover.rs       circuit breaker, adapters (sitemap/RSS/API/search/CDX), filters
  ranker.rs         in-process hybrid BM25 + Tranco + freshness scorer
  extract.rs        Readability + SSR/JSON-LD HTML-to-Markdown extraction
  measure.rs        probabilistic source shape & AI-slop estimator + calibration
  scout.rs          autonomous domain scout + runtime Beta source trust DB
  search.rs         pipeline orchestration, domain diversification, pipeline cache
eval/
  queries.jsonl     60 judged EN/TR evaluation queries
  eval.ts           Bun evaluation & paid-provider comparison harness
```

## Licence

[AGPL-3.0-only](LICENSE).
