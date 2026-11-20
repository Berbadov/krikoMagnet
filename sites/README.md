# Site registry and adapters

`registry.yaml` is the hand-maintained topic map. Every active site was
checked live on 2026-11-19/20 with the honest user agent
`KrikoMagnet/0.2 (+https://kriko.cc/bot)`. A method is listed only when
robots.txt allows it and it returned links for a test query.

## Active sites (21)

| Site | Topics | Method | Notes |
|---|---|---|---|
| gsmarena.com | phones, reviews | sitemap (`reviews.xml`, `phones.xml`) | site search `res.php3` is disallowed |
| rtings.com | tv-audio, reviews, computing, appliances | sitemap | search page is JS-only |
| donanimhaber.com | phones, computing, cars, reviews, complaints | sitemap (monthly posts) | search query strings are disallowed |
| cpsc.gov / saferproducts.gov | recalls, appliances, phones, computing, cars | official recalls API | site search is disallowed; XML sitemap stops in 2012 |
| ifixit.com | repair, phones, computing, appliances | sitemap (guide + wiki) | 56k guide URLs |
| vaillant.co.uk | boilers, repair | sitemap (290 URLs) | fault-code and pressure guides |
| consumeraffairs.com | complaints, recalls, appliances, cars, phones | sitemap | search is JS-only |
| whathifi.com | tv-audio, reviews | sitemap (monthly) | `searchTerm` is disallowed |
| macrumors.com | apple, phones, computing, repair | sitemaps (review/how-to/guide) | `/search*` is disallowed |
| tomshardware.com | computing, reviews, phones | sitemap (monthly, full backfill in the index) | search query strings are disallowed |
| caranddriver.com | cars, reviews | sitemap (`content`) | 27k review/feature URLs |
| webtekno.com | phones, computing, reviews, complaints | sitemap (recent + evergreen) | large Turkish archive |
| chip.com.tr | phones, computing, reviews, complaints | sitemap (monthly) | Turkish tech |
| guvensizurun.ticaret.gov.tr | recalls, appliances, complaints | RSS | official GUBIS unsafe-product feed; robots.txt is an HTML error page |
| vaillant.com.tr | boilers, repair, appliances | sitemap (content/products/downloads) | Turkish kombi fault codes, pressure and maintenance guides |
| bosch-home.com.tr | appliances, repair, complaints | sitemap (sherpa index) | Turkish appliance support/help pages |
| eca.com.tr | boilers, appliances, repair | sitemap (pages/products) | Turkish kombi blog: pressure, no hot water, filter cleaning |
| baymak.com.tr | boilers, appliances, repair | sitemap | Turkish user guides (kombi water pressure) |
| donanimarsivi.com | phones, computing, reviews, complaints | sitemap (post sitemaps) | Turkish tech forum/news archive |
| shiftdelete.net | phones, computing, reviews, complaints | sitemap (news/pages) | Turkish tech news; named AI crawlers are blocked, the honest UA is allowed |
| tamindir.com | computing, phones, reviews | sitemap (news/blog/software) | Turkish software/news archive |

## General reference sources (5)

When a query hits none of the consumer topics, the planner falls back to
`general` (`topics: []` → `plan["fallback"] == "general"`). Consumer queries
never reach these sites. All five were checked live on 2026-11-20 with the
honest UA, and only robots-allowed endpoints are used:

| Site | Topics | Method | Notes |
|---|---|---|---|
| en.wikipedia.org | general | `json_search` on `api.wikimedia.org` | `/w/api.php` and `/api/rest_v1` are Disallow; the Wikimedia gateway is allowed |
| tr.wikipedia.org | general | `json_search` on `api.wikimedia.org` | Turkish Vikipedi, same gateway |
| api.crossref.org | general | `json_search` (`works?query=`) | scholarly metadata; titles come back as a one-element list |
| api.stackexchange.com (site=math) | general | `json_search` (`search/advanced`) | question bodies come from the API (`filter=withbody`) |
| api.stackexchange.com (site=stackoverflow) | general | `json_search` (`search/advanced`) | question pages themselves are robots-disallowed, so they are never fetched |

Search-engine endpoints were not used: `en.wikipedia.org/wiki/Special:Search`,
`export.arxiv.org/api`, `api.openalex.org` and `stackoverflow.com/questions/*`
were all rejected by robots.txt or dropped for budget reasons.

## Open-web fallback (Common Crawl)

`sites/openweb.py` asks the Common Crawl CDX index for URLs on a few general
domains whose **address** carries a query term
(`index.commoncrawl.org/...?url=en.wikipedia.org/*kolmogorov*`). It runs only
when the registry returns zero candidates, is capped at 8 s
(`OPEN_WEB_BUDGET_S`), and sets a 10-minute cooldown when the index is
unavailable so a dead service cannot slow every query. The index maps URLs, not
page text: it can surface a wiki article slug, never a page that merely mentions
the term. Configured under `open_web` in `registry.yaml`.

## Skipped, with reasons

| Site | Why |
|---|---|
| sikayetvar.com | 429 / CAPTCHA on every honest-UA request; no sitemap. Not bypassed. |
| technopat.net | Cloudflare 403 |
| sahibinden.com | Cloudflare 403 |
| trendyol.com | search query strings disallowed; sitemap endpoint 403 |
| hepsiburada.com | 403 |
| nhtsa.gov | 403 |
| old.reddit.com | robots.txt: `Disallow: /` |
| techpowerup.com | robots.txt forbids automated data mining/LLM use |
| plumbersforums.net, repairclinic.com, edmunds.com, motor1.com | 403 / bot challenge |
| trustpilot.com | challenge; reviews/search disallowed |
| notebookcheck.net | search is Google CSE; only a rolling 5,000-URL sitemap |
| support.apple.com | sparse sitemap children and 429s |
| arabam.com | search disallowed; 87 × ~10 MB classified-ad sitemaps |
| partselect.com | sitemap allowed, model pages return Cloudflare 403 |
| ec.europa.eu Safety Gate | JS-only SPA, no sitemap or public search endpoint found |
| otoclubturkiye.com | forum search is Google CSE |
| autocar.co.uk | 202 bot challenge |
| arcelik.com.tr, beko.com.tr | robots.txt returns 403 to the honest UA → deny-all, not fetched |
| r10.net | robots.txt 403 → deny-all |
| log.com.tr | robots.txt `Disallow: /` for all agents |
| vestel.com.tr | product sitemaps live on `statics.vestel.com.tr`, whose robots.txt returns 403 → not ingested |
| demirdokum.com.tr | advertised sitemap URL returns 404 (both schemes) |

## Adapter types

- `search` — the site's own search URL plus a CSS selector for result links.
- `sitemap` — sitemap index or urlset, optional child filtering and limits.
- `rss` — RSS/Atom-style feed items; `{query}`/`{slug}` in the feed URL is
  substituted before the request.
- `api` — an official public JSON API (currently only CPSC, fixed field names).
- `json_search` — a JSON search endpoint described by field paths
  (`items_path`, `title`, `snippet`, `url_field`/`url_template`, `date`,
  `body_field`) so reference APIs need no adapter code. `body_field` takes the
  text from the API response, which is how Stack Exchange results carry content
  without fetching the robots-disallowed question page.
- `url_pattern` — URL templates; implemented and tested, no active site relies
  on it yet.

## Local index (Stage 5)

`python -m sites.index build` ingests every sitemap/RSS into
`data/site_index.sqlite` (SQLite FTS5: url/title/snippet/lastmod + folded match
text). Query time then reads the index instead of downloading sitemaps; only
the top-k pages are fetched, still through `fetchlib` (robots.txt, honest UA,
>=1 s/host). `refresh` uses conditional GETs (ETag / If-Modified-Since) and
only the newest children; `build` is the one-time full backfill (which is what
brings older reviews such as the Jan-2024 RTX 4070 Super review back into the
candidate pool). `stats` prints page counts and age. Ingest never fetches pages,
only sitemaps/feeds.

## Query planning

The planner is keyword-only (no LLM, no search engine):

1. Detect Turkish vs English from Turkish characters and stopwords.
2. Score topics from a bilingual lexicon (`phones`, `computing`, `tv-audio`,
   `appliances`, `cars`, `boilers`, `recalls`, `repair`, `complaints`, `reviews`,
   `apple`) with Turkish terms such as `kombi`, `buzdolabı`, `bulaşık`. With no
   topic hit the plan is empty and `plan_sites` switches to the `general`
   reference sources; that fallback is reported as `fallback: general`.
3. Pick sites by exact query-term matches, brand/domain matches (e.g. `bosch`
   lifts bosch-home.com.tr), topic overlap, language, and Tranco popularity.
4. Run adapters (or the local index) concurrently within the fetch layer's
   politeness limits.
5. Merge candidates and apply model/brand/product/intent filters; Turkish and
   English product nouns are treated as equivalents (`kombi` ≈ `boiler`).
6. Send the candidate pool to the Rust ranker (BM25 + Tranco prior + freshness
   + an optional discovery score).

## Health check

```bash
python -m sites.healthcheck            # human table
python -m sites.healthcheck --json     # JSON report; non-zero exit if broken
```

One known query per adapter; reports links found, extraction status and chars,
fetches, seconds, index page counts/age, and any open circuit breaker. Run it
after changing a selector or sitemap.

## Reliability

- Per-site circuit breaker: 3 consecutive failures open the site for a 5-minute
  cooldown; discovery skips it (`circuit-open`) and the health check reports it.
- Per-adapter timeout (25 s) and graceful partial results.
- Retries with jittered backoff for transport errors; `Retry-After` is still
  honoured exactly for 429/503.
- Structured JSON logs on `KRIKO_LOG=INFO` (default WARNING).

