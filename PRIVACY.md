# Privacy

Kriko Magnet learns about **products and sources, never about people**.

## What it learns

Anonymous, aggregate counts of whether a page was useful, unhelpful or served
for a product (`learned_urls`), and per-domain totals (`domain_stats`), each
with the day it was last seen. A count affects nothing until it reaches
`MAGNET_LEARN_MIN_COUNT` (k, default 3). Counts halve every 90 days and rows
older than 180 days are deleted. Details are in
[README.md](README.md#learning-and-privacy).

## What it never stores

- people, user IDs, accounts, devices, install or advertising IDs, cookies,
  sessions;
- IP addresses;
- timestamps finer than a day in learned data;
- free-text queries in learned data;
- per-request hints (`prefer_domains`, `avoid_domains`, `lang`): they shape
  one response and are never written to a table, cache key or log line;
- URLs with query strings, fragments, or personal or transactional paths
  (accounts, carts, orders, payments, tracking, tokens).

Submitted items with any unknown field, such as `user_id`, `device`, `ip`,
`email` or `session`, are rejected whole.

## Personalisation

Per-user preferences are designed to live on the user's own device and to be
sent as ephemeral hints with each search. Magnet keeps no server-side profile.

## No targeted ads

Learned data describes products and sources only and cannot be joined to a
person. It must not be sold, shared for advertising, or used for ad targeting
or profiling.
