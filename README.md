# Kriko Magnet

Engine-free, polite search for Kriko: given a product, review, recall, or
troubleshooting query, it finds and extracts pages directly from the sites that
own the answer without calling Google, Bing, SearXNG, or any search API.

Rewritten as a single minimal Rust binary (`magnet`) with Bun TypeScript tooling
(`eval/eval.ts`).

## Architecture

1. **Plan** ([src/planner.rs](src/planner.rs)) — keyword/language query planner
   routes English and Turkish consumer queries across 21 sites in
   [registry.yaml](registry.yaml), falling back to 5 general reference sources
   (Wikipedia EN/TR, Crossref, Stack Exchange) when no consumer topic matches.
2. **Discover** ([src/index.rs](src/index.rs), [src/discover.rs](src/discover.rs)) —
   sitemap/RSS sites answer from a local SQLite FTS5 index (`data/site_index.sqlite`);
   APIs and JSON endpoints run live; Common Crawl URL-index fallback runs only if
   the registry finds nothing.
3. **Rank** ([src/ranker.rs](src/ranker.rs)) — in-process BM25 + Tranco
   authority prior + exponential freshness + discovery score.
4. **Fetch & Extract** ([src/http.rs](src/http.rs), [src/extract.rs](src/extract.rs)) —
   honest `KrikoMagnet/0.2` user agent, `robots.txt` enforced and cached, >=1 s
   per host + `Crawl-delay`, conditional GET, `Retry-After` backoff, pure-Rust
   Readability + Markdown extraction, and SQLite WAL caches.
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

# CLI search, fetch, health, and source quality estimation
./target/release/magnet search "lg c1 burn in" -k 5
./target/release/magnet health
./target/release/magnet calibrate
./target/release/magnet measure https://example.com/a https://example.com/b

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
  extract.rs        Readability + HTML-to-Markdown extraction
  measure.rs        probabilistic source shape & AI-slop estimator + calibration
  search.rs         pipeline orchestration, domain diversification, pipeline cache
eval/
  queries.jsonl     60 judged EN/TR evaluation queries
  eval.ts           Bun evaluation & paid-provider comparison harness
```

## Licence

[AGPL-3.0-only](LICENSE).
