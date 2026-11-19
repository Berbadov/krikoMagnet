# Stage 2 — Direct Website Fetching & Block-Rate Telemetry

Measures real-world HTTP success vs anti-bot blocks when fetching target sites
**directly** — no proxies, no headless browser.

## Files
| File | Purpose |
|---|---|
| `urls.json` | The 40-target roster (verified-valid URLs) across categories: news/review blogs, manufacturer support, technical docs, forums/communities, Reddit threads, iFixit guides, product-review blogs |
| `pilot.py` / `status_check.py` | Preliminary connectivity/status probes (used to curate valid URLs) |
| `fetch_bench.py` | The telemetry benchmark |

## How it works (`fetch_bench.py`)
- Concurrency = 8 via `curl_cffi.AsyncSession(impersonate="chrome")`.
- Extraction: `trafilatura.extract(...)`; fallback to `selectolax` body text when trafilatura yields <300 chars.
- Classification:
  - `hard-block`: HTTP 403 / 429 / 401.
  - `soft-block`: HTTP 200 but <300 clean chars **or** a challenge marker in the *extracted text* (matching raw JS payloads caused false positives — see below).
  - `http-error`: other non-200.
  - `success`: HTTP 200, ≥300 clean chars, no challenge marker.
- Prints per-domain telemetry (domain, category, status, clean chars, fetch/parse ms, verdict), then aggregate success/block rates and the gate verdict.

## Gate 2 result (PASS)
| Metric | Value | Gate |
|---|---|---|
| Clean extraction success | **80.0%** (32/40) | ≥ 75% ✅ |
| Hard/soft block rate | **17.5%** (7/40) | ≤ 25% ✅ |
| HTTP errors (non-block) | 1 (Ars Technica 405) | — |

## Blocked / degraded (honest)
- **Hard:** `serverfault.com` → 403 (Cloudflare).
- **Soft:** `support.hp.com` (0 chars), `www.quora.com` (118), `www.reddit.com` ×4 (144) — JS-shell / challenge pages.

## Note on classifier accuracy
The first marker scan matched challenge words ("captcha", "access denied",
"just a moment") against the **raw HTML**, which false-flagged successfully
extracted pages (apnews.com → 4,917 clean chars, Lenovo → 5,533). Markers are
now tested against the **extracted text only**, so a real challenge/render
outcome is what flags a soft-block.

## Run
```bash
python fetch_bench.py
```
