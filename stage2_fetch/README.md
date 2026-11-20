# Stage 2 — Polite Direct Fetching & Block-Rate Telemetry

Measures real-world HTTP success vs anti-bot blocks when fetching target sites
**directly** — no proxies, no headless browser, no search-engine provider.

## Files
| File | Purpose |
|---|---|
| `urls.json` | The 40-target roster across news/review blogs, manufacturer support, technical docs, forums/communities, Reddit threads, iFixit guides, product-review blogs |
| `fetch_bench.py` | The telemetry benchmark, using the shared `fetchlib` layer |
| `pilot.py` / `status_check.py` | Preliminary connectivity/status probes used to curate the roster |

## How it works (`fetch_bench.py`)
- One shared `fetchlib.Fetcher`, concurrency cap 8.
- **Honest identity:** default UA `KrikoMagnet/0.2 (+https://kriko.cc/bot)`;
  Chrome TLS impersonation is off by default.
- **robots.txt:** cached, parsed with protego (wildcards/Crawl-delay); a
  disallowed path is never fetched and is reported as `robots-denied`.
- **Politeness:** >=1 s between requests per host, global cap, 429/Retry-After
  backoff, response TTL cache.
- Extraction: `trafilatura.extract(...)`; fallback to selectolax/lexbor body
  text when trafilatura yields <300 chars.
- Classification:
  - `hard-block`: HTTP 403 / 429 / 401.
  - `soft-block`: HTTP 200 but <300 clean chars **or** a challenge marker in the
    *extracted text*.
  - `http-error`: other non-200.
  - `success`: HTTP 200, >=300 clean chars, no challenge marker.
- robots-denied targets are excluded from the success/block denominator and
  reported separately.

## Gate 2 result (PASS, polite run)
| Metric | Value | Gate |
|---|---|---|
| Clean extraction success (fetchable) | **81.8%** (27/33) | >= 75% PASS |
| Hard/soft block rate (fetchable) | **15.2%** (5/33) | <= 25% PASS |
| HTTP errors (non-block) | 1 (Ars Technica 405) | — |
| robots.txt skips | 7 (Reddit ×4, serverfault, stackexchange, Lenovo) | — |

## Blocked / degraded (honest)
- **Hard:** `apnews.com`, `realpython.com`, `www.quora.com`,
  `www.sitepoint.com` → 403.
- **Soft:** `forums.tomshardware.com` (89 chars).
- **HTTP error:** `arstechnica.com` → 405.
- **robots.txt:** `serverfault.com`, `stackexchange.com`, `www.lenovo.com`,
  `www.reddit.com` ×4 — never fetched.

## Note on classifier accuracy
Markers are tested against the **extracted text only**, so a successfully
extracted page that merely mentions "captcha" in a script is not false-flagged.

## Run
```bash
python stage2_fetch/fetch_bench.py
```
