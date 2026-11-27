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
   with sequential Bayesian stopping. `magnet scout ... --promote` only stages
   additions and skips in `registry.pending.yaml` for review
   (`diff registry.yaml registry.pending.yaml`); `--promote --apply
   [--build-index]` edits `registry.yaml` directly.
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

## Sufficiency Signal

Every search response (HTTP, CLI and `magnet_search`) carries a calibrated
"nothing good found" verdict so callers know when to fall back to a paid
search API:

```json
{ "sufficient": false, "confidence": 0.222,
  "reason": "results only partly cover the query terms",
  "confidence_signals": { "results": 5, "usable": 5, "matching": 0,
    "best_coverage": 0.667, "concepts": 3, "identity_terms": ["lg", "c1"] } }
```

How it is computed ([src/confidence.rs](src/confidence.rs)):

1. **Concepts** are the query terms minus stopwords and generic intent words
   (`review`, `recall`, `inceleme`, ...), stemmed, each with its TR/EN product
   aliases. **Identity concepts** are brands, model codes (`c1`, `f28`,
   `1000xm5`) and bare numbers (`iphone 18`).
2. A result is **usable** when it was fetched with HTTP 200 and at least 300
   extracted characters (any result when `fetch=false`).
3. A usable result **matches** when every identity concept appears in its URL,
   title, snippet or first 500 extracted characters, and at least 66% of all
   concepts appear in those fields plus the first 4000 extracted characters.
4. `confidence = best_coverage × min(1, (matches + 1) / 3)`; one full match
   scores `0.667`, two or more score their coverage. It is a graded score for
   callers that want their own cut-off.
5. `sufficient = matches ≥ 1`. When it is false, `reason`
   says why: `no results`, `no result could be fetched and extracted`,
   `no result names the brand, model or number in the query`, or
   `results only partly cover the query terms`.

Calibration on the cached top-5 payloads of the 66 eval queries, labelled with
[eval/queries.jsonl](eval/queries.jsonl) and
[eval/open_qrels.jsonl](eval/open_qrels.jsonl):

| Set | Expected | Result |
|---|---|---|
| Queries with a judged grade-1/2 hit (`59`) | sufficient | `59` (`100%`) |
| Query scored against another query's results (`66`) | insufficient | `63`; two of the three accepts are relevant cross-language pairs (`iphone ekran değişimi` ← iPhone screen-replacement pages, `vaillant ecotec fault code f28` ← Turkish Vaillant F28 pages), one is borderline (`vaillant boiler pressure too low` ← Vaillant "pressure too high" pages) |
| Same-brand near misses, e.g. `lg c2` ← `lg c1` pages (`18`) | insufficient | `16`; both accepts are the LG C1/C2 burn-in pair, whose titles compare the two models |

Keeping only each positive query's top result, `57/59` stay sufficient. A `75%`
coverage threshold rejects one more near miss but also marks the exact Vaillant
F28 fault-code page insufficient for `vaillant ecotec fault code f28`.

`bun run eval` re-checks `sufficient` against the qrels on a live run.

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
DeepSeek + Linkup, `$0.00` for Magnet).

#### Blind Open-Web Pooled Relevance (Domain-Agnostic & Language-Agnostic)

Scoring open-web APIs against [eval/queries.jsonl](eval/queries.jsonl) alone
forces off-registry hits to `0` (`91.4%` of DeepSeek URLs and `95.5%` of Linkup
URLs fall outside Magnet's `21` curated domains). To eliminate that closed-set
bias, all `682` unique `(query, url)` pairs from top-5 results across
`magnet_warm_fetch`, `deepseek`, and `linkup` were pooled, verified by live HTTP
fetches, and graded blindly without provider attribution (`2` = direct
authoritative answer on **any domain in any language**, `1` = partial/sibling or
secondary blog/complaint, `0` = off-topic/dead `4xx/5xx/ENOTFOUND`/soft-redirect/spam;
archived in [eval/open_qrels.jsonl](eval/open_qrels.jsonl)). Canonical URL
fragments (`#1`, `#3`) are deduplicated within top-5 and normalized against the
shared pooled ideal `IDCG@5`.

| Provider | Hit@1 (`2`) | Hit@5 (`2`, 95% CI) | MRR@5 | Pooled nDCG@5 (95% CI) | Clean200 nDCG@5 | Lang-Match | Clean HTTP-200 | p50 / p95 (ms) | Cost (`/1k`) |
|---|---:|---|---:|---|---|---|---|---|---:|
| **`magnet` (cold, 15s)** | `81.6%` | `91.8% [83.7–98.0%]` | `0.859` | `0.594 [0.534–0.660]` | `0.603` | `84.8%` | `100.0%` | `7930 / 15059` | `$0.00` |
| **`magnet_warm_fetch`** | **`85.7%`** | **`98.0% [93.9–100.0%]`** | **`0.905`** | `0.643 [0.593–0.696]` | **`0.656`** | `84.7%` | **`100.0%`** | `457 / 12052` | `$0.00` |
| **`deepseek`** | `65.3%` | `85.7% [75.5–95.9%]` | `0.730` | **`0.662 [0.592–0.732]`** | `0.594` | `61.6%` | `75.1%` | **`664 / 1015`** | `$0.75` |
| **`linkup`** | `42.9%` | `81.6% [69.4–91.8%]` | `0.584` | `0.592 [0.518–0.666]` | `0.435` | **`89.8%`** | `65.7%` | `1632 / 2115` | `$6.00` |

- `magnet_warm_cache` serves identical rankings at `1 ms` p50 / `10 ms` p95 (`459.9 QPS`, `$0.00/1k`).
- **Paired Win / Tie / Loss (`49` judged, strict pooled `nDCG@5`):** `magnet_warm_fetch` vs `deepseek` = `21 / 1 / 27` (statistically tied inside overlapping CIs; `24 / 1 / 24` on `Clean HTTP-200 nDCG@5`); `magnet_warm_fetch` vs `linkup` = `28 / 0 / 21` (`34 / 2 / 13` on `Clean HTTP-200`).
- **Rank-1 Precision vs Open-Web Tradeoffs:**
  - **Kriko Magnet** leads **rank-1 precision** (`85.7% Hit@1`, `0.905 MRR@5`), **direct-answer coverage** (`98.0% Hit@5`, `100.0% rel>=1`), **`100.0%` clean HTTP-200 extractability**, and **`$0.00` API spend**, while fixed 21-site breadth causes lower `P@5` in ranks 3–5 (`48.2%` vs DeepSeek `51.0%`) and slower uncached cold fetches.
  - **DeepSeek (`0.662 nDCG@5`)** finds relevant pages across `22` languages (`$0.75/1k`, `664 ms` p50), but drifts off query language (`61.6%` overall, `52.7%` on Turkish queries) and returns `24.9%` non-clean-HTTP links (`11` dead, `5` soft-redirects, `45` gated/403/timeout/PDF).
  - **Linkup (`89.8%` target-language match)** localizes much better than DeepSeek, but `34.3%` of URLs fail direct HTTP-200 fetching (`Clean200 nDCG@5 = 0.435`), and `76 / 245` (`31.0%`) of slots are local repair lead-gen, AI affiliate roundups (`deal.ai`), or syndicate clones (`LEVERSNET`). Under a **Permissive rubric** promoting those derivative pages to `grade=2`, Linkup scores `95.9% Hit@5` / `0.737 nDCG@5` (`15 / 2 / 32` vs Magnet).

#### Closed Curated-Registry URL Recall (`eval/queries.jsonl` Only)

Strictly measures exact URL recovery inside Magnet's `21` curated domains:

| Provider | Hit@1 | Hit@5 (95% CI) | MRR@5 | nDCG@5 (95% CI) | Domain-Hit@5 | p50 / p95 (ms) | QPS | Cost (`/1k`) |
|---|---:|---|---|---:|---:|---|---:|---:|
| **`magnet` (cold, 15s budget)** | `55.1%` | `83.7% [73.5–93.9%]` | `0.657` | `0.640 [0.533–0.729]` | `91.8%` | `7930 / 15059` | `0.1` | `$0.00` |
| **`magnet_warm_fetch`** | `61.2%` | `91.8% [83.7–98.0%]` | `0.723` | `0.698 [0.603–0.783]` | `98.0%` | `457 / 12052` | `0.4` | `$0.00` |
| **`magnet_warm_cache`** | `61.2%` | `91.8% [83.7–98.0%]` | `0.723` | `0.698 [0.603–0.783]` | `98.0%` | `1 / 10` | `459.9` | `$0.00` |
| **`deepseek`** | `6.1%` | `6.1% [0.0–14.3%]` | `0.061` | `0.053 [0.000–0.119]` | `16.3%` | `664 / 1015` | `1.4` | `$0.75` |
| **`linkup`** | `4.1%` | `12.2% [4.1–20.4%]` | `0.069` | `0.068 [0.018–0.127]` | `16.3%` | `1632 / 2115` | `0.6` | `$6.00` |

- Under a strict `15 s` cold HTTP timeout in `fetch_many`, aborted page fetches omit those candidates from `results` (`83.7%` closed / `91.8%` open cold vs `91.8%` closed / `98.0%` open once fetched).

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
  queries.jsonl     60 EN/TR benchmark queries (59 judged closed-set qrels)
  open_qrels.jsonl  682 blind pooled open-web graded qrels across Magnet/DeepSeek/Linkup
  eval.ts           Bun evaluation & paid-provider comparison harness
```

## Licence

[AGPL-3.0-only](LICENSE).
