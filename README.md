# Kriko Magnet

Engine-free, polite search for the Kriko ("check before you buy") apps: given
a product, review or troubleshooting query, it finds the exact page on the
sites that own the answer without calling Google, Bing, SearXNG or any search
API. A hand-verified site registry and bilingual topic map pick sources; each
source is queried through its sitemap, RSS feed, own search page or official
API; a local SQLite FTS5 index of sitemap/RSS metadata makes discovery a
lookup instead of a crawl; a Rust BM25 + Tranco + freshness ranker orders the
candidates; and a shared polite fetch layer (robots.txt enforced, honest
`KrikoMagnet/0.2` user agent, >=1 s per host, no impersonation, no CAPTCHA
solving) fetches only the top results, extracts them with trafilatura, and
caches the text in SQLite.

## Results

Stage 5, 60 judged queries (59 scored), k=5. Stage 4 is the previous 42-query
run, so the sets differ; the same-query comparison is at the end.

| Metric | Stage 4 (41 judged) | Stage 5 (59 judged) |
|---|---|---|
| Queries with a relevant result in top-5 | 92.7% | **94.9%** |
| nDCG@5 | 0.763 | 0.738 |
| nDCG@5 English | 0.821 | 0.789 |
| nDCG@5 Turkish | 0.650 | **0.681** |
| Cold p50 / p95 | 6462 / 10627 ms | **3159 / 7336 ms** |
| Warm-fetch, cold-query p50 | 1001 ms | **580 ms** |
| Warm query cache p50 | 1 ms | **0.4 ms** |
| Network fetches per query (cold) | 7.2 | **4.2** |
| Sites / indexed pages | 14 / none | 21 / 913,284 |

On the identical 41 judged queries from Stage 4, hit@5 is unchanged at 92.7%
and cold p50 drops from 6.5 s to 3.2 s, but nDCG falls to 0.657: the wider
index surfaces many near-duplicate Turkish phone pages, and three exact-URL
judgments that used to earn partial credit now score zero. Full tables,
failure notes and the method are in [RESULTS.md](RESULTS.md).

### Firecrawl comparison

One benchmark (2026-11-20) on Magnet's own judged queries and the same 100
pages, with the same robots pre-check, honest UA and per-host spacing applied
to both sides. Firecrawl was self-hosted from `main`, no cloud key, no LLM.

| | Firecrawl (self-hosted + SearXNG) | Magnet |
|---|---|---|
| Pages with >=300 chars of text | 98/100 | 94/100 |
| Extraction F1 vs page's `<article>`/`<main>` (n=60) | 0.43 | 0.57 |
| End-to-end hit@5 (same 59 queries) | 45.8% | 79.7% |
| nDCG@5 | 0.33 | 0.70 |
| Hit@5 Turkish | 39% | 64% |
| Whole-query latency | 18 s | 5 s |
| Footprint | 6 containers, ~3.4 GB RAM | one Python process + one Rust binary |

This is one benchmark, on Magnet's own queries, not a general web-search
comparison. Magnet covers 21 hand-picked sites; Firecrawl's `/search` is a
proxy in front of public engines (SearXNG -> DuckDuckGo/Mojeek), so it can
reach the whole web but inherits that dependency, those rankings and that
traffic. Firecrawl's extraction is the opposite trade: it returns more of the
article but also the navigation (18% boilerplate lines vs 1.2%), and it does
not check robots.txt on `/scrape` by default. The Magnet arm in that table is
the Stage-5 run of that day; the current rerun scores 94.9% / 0.738 at 3.16 s.
Firecrawl's server is AGPL-3.0.

## How it works

1. **Plan** - keyword-only query planner detects Turkish/English and scores
   topics from a bilingual lexicon (`kombi`, `buzdolabı`, `bulaşık`, `recall`, ...).
2. **Discover** - the registry picks up to six sites; sitemap/RSS sites are
   answered from a local FTS5 index (`url`, `title`, `lastmod`, match text),
   while site search and official APIs run live.
3. **Rank** - a resident Rust process scores BM25 over title+snippet + a
   log-scaled Tranco authority prior + freshness + the discovery score.
4. **Fetch** - top results are fetched concurrently across hosts, never more
   than one request per second to the same host, robots.txt checked and cached.
5. **Extract and cache** - trafilatura (selectolax fallback) to markdown;
   query answers, page text, robots rules and HTTP responses live in SQLite.

## Quick start

Python 3.14+ and Rust 1.96+.

```bash
python -m pip install -r requirements.txt
cargo build --release --manifest-path stage1_rs/Cargo.toml

# One-time polite sitemap/RSS backfill (~20 min, ~900k pages).
python -m sites.index build

# Serve searches.
python stage3_pipeline/app.py          # http://127.0.0.1:8000
curl -X POST localhost:8000/search -H 'Content-Type: application/json' \
     -d '{"query":"lg c1 burn in","k":5}'

# Honest evaluation on 60 judged queries (needs the index).
python eval/run_eval.py --fresh --json eval/results_stage5.json \
  --markdown eval/results_stage5.md

# Adapter health, index age and circuit state; non-zero exit on a break.
python -m sites.healthcheck --json
```

`python -m sites.index refresh` does a conditional (ETag / If-Modified-Since)
refresh in the background; the FastAPI app can run it periodically with
`KRIKO_REFRESH_MINUTES=N`. A total-query deadline is available as
`--page-budget MS` (off by default: it returns partial results but costs
recall).

## Politeness

- robots.txt is fetched, cached 24 h and enforced; a disallowed path is never
  fetched. `401/403/429/5xx` robots responses mean deny-all.
- Honest user agent: `KrikoMagnet/0.2 (+https://kriko.cc/bot)`. No TLS
  impersonation, no proxies, no headless browser, no CAPTCHA solving.
- At least one second between requests to the same host, `Crawl-delay`
  honoured, global concurrency capped, `429`/`Retry-After` respected.
- Sites that block the honest agent (Cloudflare, 403 robots, CAPTCHA) are
  recorded as skipped, not worked around.

## Limitations

- **21 sites, not the web.** Queries outside the registry have no answer.
- **English and Turkish only**, with the best coverage for phones, TVs,
  computing, appliances, boilers, cars, recalls and repair.
- **Generic Turkish phone queries are the weak spot**: newer coverage often
  outranks the narrowly judged page, and the exact-URL eval gives no credit
  for near misses.
- **Cold latency is bounded by politeness.** Five results on one host cost
  several one-second slots; warm fetches are ~0.6 s and repeated queries
  ~0.4 ms.
- **Sitemap coverage is not complete.** Six DonanımHaber monthly sitemaps
  returned `429` during the backfill and were not bypassed; a later refresh
  can fill them.
- **Ranking is a hand-tuned hybrid**, not a learned ranker.

## Docs

- [RESULTS.md](RESULTS.md) - all stage results, metrics and failure notes
- [sites/README.md](sites/README.md) - registry, adapters, local index, health
- [fetchlib/README.md](fetchlib/README.md) - robots, rate limits, caching, extraction
- [eval/README.md](eval/README.md) - judged queries and how to read the numbers
- [stage3_pipeline/README.md](stage3_pipeline/README.md) - pipeline, cache, resident ranker
- [stage1_rs/README.md](stage1_rs/README.md) - the Rust ranker

An MCP server for Magnet lives on the unmerged
[`feat/mcp-server`](https://github.com/Berbadov/krikoMagnet/tree/feat/mcp-server)
branch.

## Licence

[AGPL-3.0-only](LICENSE). The author is the sole copyright holder and may
license this code elsewhere; running a modified version as a network service
requires offering its source to that service's users (AGPL-3.0 section 13).
Runtime dependencies are permissively licensed (MIT, Apache-2.0, BSD); there
are no GPL/AGPL runtime dependencies.
