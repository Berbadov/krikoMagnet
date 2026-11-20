# Shared fetch layer

`fetchlib` is used by Stage 2, Stage 3, and every site adapter.

- **Honest identity:** default UA is
  `KrikoMagnet/0.2 (+https://kriko.cc/bot)`. Chrome TLS impersonation is
  **off by default**. It exists only as an explicit opt-in
  (`Fetcher(impersonate="chrome")`) for a site that cannot be read otherwise;
  enabling it weakens the "honest bot" claim and must be documented when used.
- **robots.txt:** fetched once per origin and cached for 24 h. Uses protego
  (wildcards, Crawl-delay) with a urllib fallback. A disallowed path is never
  fetched. `401/403/429/5xx` robots responses are treated as deny-all; a 404 or
  an HTML error page is treated as "robots unavailable" (allow, but recorded).
- **Politeness:** at least 1 s between requests to the same host
  (`min_interval` configurable), Crawl-delay honoured, global async concurrency
  cap (default 8).
- **429/503:** `Retry-After` (seconds or HTTP date) is honoured, capped at 30 s,
  with at most two retries.
- **Caching:** HTTP responses in SQLite with a TTL (default 24 h for pages,
  24 h for robots). Sitemaps are additionally parsed once per process.
- **Extraction:** `fetchlib.extract_text` uses trafilatura with a selectolax
  (lexbor) fallback; `classify_fetch` gives the Stage-2 hard/soft block verdict.

## Tests

`tests/test_fetch.py` covers robots denial (including wildcards), 403 robots,
HTML robots errors, rate limiting with an injected clock, Retry-After
backoff, and TTL cache behaviour. No network is used.

## Tranco download

`stage1_py/tranco_cache.py` uses the S3 origin
(`tranco-list.s3.amazonaws.com`) and an honest UA. The original 525 was an SSL
handshake failure at `tranco-list.eu`, not a bot wall; no impersonation is
needed for the S3 origin.
