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
./target/release/magnet scout plato.stanford.edu --sample 6

# HTTP server (http://127.0.0.1:8000)
./target/release/magnet serve

# Stdio MCP server (for OpenCode / MCP clients)
./target/release/magnet mcp

# Evaluation & provider benchmark via Bun
bun run eval:fresh
bun run bench -- --dry-run
```

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
