# Magnet vs DeepSeek Search vs Linkup

- Queries: 50 drawn from `eval/queries.jsonl` (49 with exact-URL judgments)
- Cutoff: top-5; judgments are the existing Kriko ones, matched exactly after URL normalisation (no LLM judging)
- Providers: Magnet (this repo, fresh caches), DeepSeek Search API (`/v1/tools/search`, `need_content:true`), Linkup `/v1/search` (`depth=standard`, first 5 of the returned list)

## Headline

| Provider | hit@5 | nDCG@5 | P@5 | domain-hit@5 | p50 | p95 | empty | errors | cost | $/hit |
|---|---|---|---|---|---|---|---|---|---|---|
| magnet | 83.7% | 0.618 | 0.23 | 91.8% | 4102 ms | 15020 ms | 0 | 0 | $0.0000 | $0.000 |
| deepseek | 6.1% | 0.053 | 0.01 | 20.4% | 659 ms | 857 ms | 0 | 0 | $0.0375 | $0.012 |
| linkup | 14.3% | 0.083 | 0.03 | 18.4% | 1672 ms | 1986 ms | 0 | 0 | $0.3000 | $0.043 |

*domain-hit@5 is a softer secondary signal: a judged domain appears in the top 5 even when the exact URL differs.*

Magnet's network cost in this run: 4.5 fetches and 1330 KB per query, 66.5 MB in total, 4.1 results per query.

## Reading these numbers

- **Home field.** The judgments are Kriko's own: exact URLs on review, complaint, recall and repair sites that the registry was built around. The paid engines are asked to land on the exact page a curated source list was designed to produce, the hardest framing for them. Read this as a domain benchmark, not a general web-search ranking.
- **Exact URLs only.** A near-miss page scores zero, so `domain-hit@5` sits next to it: a provider that returns the right site but the wrong article still shows up there.
- **Latency is not like-for-like.** Magnet pays for robots.txt, per-host spacing and page extraction on a cold run; the paid APIs return prepared snippets. Magnet's p50 is seconds, theirs sub-second.
- **Cost is list price, one call per query.** Magnet's $0 is self-hosted compute and bandwidth, not free — the run reports its own fetch and byte counts above.

## By language

| Provider | set | hit@5 | nDCG@5 | p50 |
|---|---|---|---|---|
| magnet | en (27) | 85.2% | 0.656 | 4863 ms |
| magnet | tr (22) | 81.8% | 0.571 | 3470 ms |
| deepseek | en (27) | 0.0% | 0.000 | 677 ms |
| deepseek | tr (22) | 13.6% | 0.119 | 636 ms |
| linkup | en (27) | 14.8% | 0.084 | 1642 ms |
| linkup | tr (22) | 13.6% | 0.082 | 1733 ms |

## Head-to-head (hit@5, per query)

- magnet vs deepseek: **38 W / 11 T / 0 L**
- magnet vs linkup: **35 W / 13 T / 1 L**

## Method

- Same query text, same k=5, one live request per query per provider.
- Magnet runs the actual pipeline (local SQLite FTS index, planner, Rust ranker, polite fetch with a 15 s page budget) using fresh caches, so its numbers are a first-run-in-a-session, not a warm cache.
- DeepSeek and Linkup are called directly, one query at a time, with up to three attempts on 429/5xx; failures are counted, not hidden.
- Cost uses list prices captured with the run: DeepSeek Search $0.75/1k successful requests, Linkup standard search $0.006/request, Magnet $0 (self-hosted; the run reports its own fetch and byte counts).

## Misses (no judged URL in top 5)

### magnet (8)

- `q01` lg c1 burn in → whathifi.com, whathifi.com
- `q06` iphone 15 pro max battery life review → macrumors.com, macrumors.com, donanimhaber.com
- `q24` best budget monitor for ps5 → tomshardware.com
- `q27` vaillant ecotec fault code f28 → vaillant.com.tr
- `q29` iphone ekran çizgi → chip.com.tr, shiftdelete.net, shiftdelete.net
- `q33` iphone pil sağlığı gece şarj → shiftdelete.net, shiftdelete.net, webtekno.com
- `q40` iphone ekran değişimi → webtekno.com, webtekno.com, shiftdelete.net
- `q47` eca kombi su ısıtmıyor → eca.com.tr, eca.com.tr, eca.com.tr

### deepseek (46)

- `q01` lg c1 burn in → tek.no, computerbase.de, hardware.com.br
- `q02` lg c1 best settings → digitalfoundry.net, oledcontrol.xyz, oledcontrol.xyz
- `q03` samsung qn90b review → samsung.com, abt.com, euronics.it
- `q04` sony wh-1000xm5 review → sony.com, sony.com.ar:443, soundguys.com
- `q05` best oled tv for movies → tech.yahoo.com, tech.yahoo.com, tech.yahoo.com
- `q06` iphone 15 pro max battery life review → macobserver.com, amazon.co.uk, shiftdelete.net
- `q07` samsung galaxy s24 ultra camera review → samsung.com, amateurphotographer.com, phonearena.com
- `q08` google pixel 8 pro overheating review → computerbase.de, galaxus.at, news.abplive.com
- `q09` iphone 15 screen replacement → support.apple.com, support.apple.com, support.apple.com
- `q10` airpods pro 2 crackling noise → soundguys.com, coolblue.de, discussionskorea.apple.com
- `q11` iphone battery draining after update → tech.yahoo.com, iphonelife.com, makeuseof.com
- `q12` macbook air m2 battery replacement → support.apple.com, support.apple.com, support.apple.com
- `q13` steam deck ssd upgrade → tech.yahoo.com, slashgear.com, asurion.com
- `q14` anker power bank recall → anker.com, anker.com, anker.com
- `q16` samsung galaxy note 7 recall → cbc.ca, cbc.ca, bbc.co.uk
- … 31 more

### linkup (42)

- `q01` lg c1 burn in → techspot.com, historytools.org, reddit.com
- `q02` lg c1 best settings → petematheson.com, tvsbook.com, pixelbeat.org
- `q03` samsung qn90b review → techtudo.com.br, wired.com, fr.windows.day
- `q04` sony wh-1000xm5 review → headphonesaddict.com, speakerranking.com, sony.com.au
- `q05` best oled tv for movies → deal.ai, deal.ai, deal.ai
- `q06` iphone 15 pro max battery life review → shopfox.ai, backmarket.com, netshopers.com
- `q07` samsung galaxy s24 ultra camera review → wibutech.com, dxomark.com, techpp.com
- `q08` google pixel 8 pro overheating review → howtogeek.com, reddit.com, androidpolice.com
- `q09` iphone 15 screen replacement → displaycellpros.com, sdip.cloudsix.io, sdindianaphones.com
- `q11` iphone battery draining after update → hawkdive.com, jonathansblog.co.uk, digitbin.com
- `q13` steam deck ssd upgrade → beebom.com, tech-insider.org, overclock3d.net
- `q14` anker power bank recall → anker.com, anker.com.sg, techwiz.blog
- `q16` samsung galaxy note 7 recall → verdictvictory.com, pages.samsung.com, businessinsider.com
- `q17` peloton treadmill recall → aboutlawsuits.com, cbsnews.com, runnersworld.com
- `q18` samsung refrigerator ice maker problem → samsung.com, samsung.com, easybear-appliancerepair.com
- … 27 more

