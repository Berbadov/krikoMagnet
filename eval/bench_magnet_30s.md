# Magnet

- Queries: 50 drawn from `eval/queries.jsonl` (49 with exact-URL judgments)
- Cutoff: top-5; judgments are the existing Kriko ones, matched exactly after URL normalisation (no LLM judging)
- Providers: Magnet (this repo, fresh caches)

## Headline

| Provider | hit@5 | nDCG@5 | P@5 | domain-hit@5 | p50 | p95 | empty | errors | cost | $/hit |
|---|---|---|---|---|---|---|---|---|---|---|
| magnet | 93.9% | 0.698 | 0.25 | 98.0% | 4218 ms | 29098 ms | 0 | 0 | $0.0000 | $0.000 |

*domain-hit@5 is a softer secondary signal: a judged domain appears in the top 5 even when the exact URL differs.*

Magnet's network cost in this run: 4.4 fetches and 1358 KB per query, 67.9 MB in total, 4.3 results per query.

## Reading these numbers

- **Home field.** The judgments are Kriko's own: exact URLs on review, complaint, recall and repair sites that the registry was built around. The paid engines are asked to land on the exact page a curated source list was designed to produce, the hardest framing for them. Read this as a domain benchmark, not a general web-search ranking.
- **Exact URLs only.** A near-miss page scores zero, so `domain-hit@5` sits next to it: a provider that returns the right site but the wrong article still shows up there.
- **Latency is not like-for-like.** Magnet pays for robots.txt, per-host spacing and page extraction on a cold run; the paid APIs return prepared snippets. Magnet's p50 is seconds, theirs sub-second.
- **Cost is list price, one call per query.** Magnet's $0 is self-hosted compute and bandwidth, not free — the run reports its own fetch and byte counts above.

## By language

| Provider | set | hit@5 | nDCG@5 | p50 |
|---|---|---|---|---|
| magnet | en (27) | 100.0% | 0.775 | 4334 ms |
| magnet | tr (22) | 86.4% | 0.604 | 3939 ms |

## Head-to-head (hit@5, per query)


## Method

- Same query text, same k=5, one live request per query per provider.
- Magnet runs the actual pipeline (local SQLite FTS index, planner, Rust ranker, polite fetch with a 30 s page budget) using fresh caches, so its numbers are a first-run-in-a-session, not a warm cache.
- DeepSeek and Linkup are called directly, one query at a time, with up to three attempts on 429/5xx; failures are counted, not hidden.
- Cost uses list prices captured with the run: DeepSeek Search $0.75/1k successful requests, Linkup standard search $0.006/request, Magnet $0 (self-hosted; the run reports its own fetch and byte counts).

## Misses (no judged URL in top 5)

### magnet (3)

- `q29` iphone ekran çizgi → chip.com.tr, shiftdelete.net, shiftdelete.net
- `q33` iphone pil sağlığı gece şarj → shiftdelete.net, shiftdelete.net, webtekno.com
- `q40` iphone ekran değişimi → webtekno.com, webtekno.com, shiftdelete.net

