# Site registry and adapters

`registry.yaml` is the hand-maintained topic map. Every active site was
checked live on 2026-11-19 with the honest user agent
`KrikoMagnet/0.2 (+https://kriko.cc/bot)`. A method is listed only when
robots.txt allows it and it returned links for a test query.

## Active sites (14)

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
| tomshardware.com | computing, reviews, phones | sitemap (monthly) | search query strings are disallowed |
| caranddriver.com | cars, reviews | sitemap (`content`) | 27k review/feature URLs |
| webtekno.com | phones, computing, reviews, complaints | sitemap (recent + evergreen) | large Turkish archive |
| chip.com.tr | phones, computing, reviews, complaints | sitemap (monthly) | Turkish tech |
| guvensizurun.ticaret.gov.tr | recalls, appliances, complaints | RSS | official GUBIS unsafe-product feed; robots.txt is an HTML error page |

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

## Adapter types

- `search` — the site's own search URL plus a CSS selector for result links.
- `sitemap` — sitemap index or urlset, optional child filtering and limits.
- `rss` — RSS/Atom-style feed items.
- `api` — an official public JSON API (currently only CPSC).
- `url_pattern` — URL templates; implemented and tested, no active site relies
  on it yet.

## Query planning

The planner is keyword-only (no LLM, no search engine):

1. Detect Turkish vs English from Turkish characters and stopwords.
2. Score topics from a small lexicon (`phones`, `computing`, `tv-audio`,
   `appliances`, `cars`, `boilers`, `recalls`, `repair`, `complaints`, `reviews`,
   `apple`).
3. Pick sites by exact query-term matches, then topic overlap, language, and
   finally Tranco popularity.
4. Run adapters concurrently within the fetch layer's politeness limits.
5. Merge candidates and apply model/brand/product/intent filters.
6. Send the candidate pool to the Rust ranker (BM25 + Tranco prior + freshness
   + an optional discovery score).

## Health check

```bash
python -m sites.healthcheck
```

One known query per adapter; reports links found, extraction status and chars,
fetches, and seconds. Run it after changing a selector or sitemap.
