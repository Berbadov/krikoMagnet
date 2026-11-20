"""Open-web fallback discovery through the Common Crawl URL index.

The registry answers consumer queries from a curated site list. When nothing in
that list matches, this module asks the Common Crawl CDX index for URLs on a few
general domains whose address carries a query term (``en.wikipedia.org/*kolmogorov*``).
The index maps URLs, not page text, so this is a discovery aid: candidates are
still scored, fetched politely and extracted like any other result.

The index is a public service that is sometimes unavailable; a short timeout and
a process-wide cooldown keep a dead index from costing every query.
"""
from __future__ import annotations

import json
import time
from urllib.parse import quote

from .adapters import Candidate, score_candidates
from .log import get_logger
from .text import fold, query_terms, unslug

log = get_logger("sites.openweb")

DEFAULT_INDEX = "https://index.commoncrawl.org/CC-MAIN-2026-43-index"
DEFAULT_DOMAINS = ("en.wikipedia.org", "arxiv.org", "github.com")
MAX_TERMS = 2
TIMEOUT_S = 6.0
COOLDOWN_S = 600.0
_GENERIC = {
    "review", "reviews", "best", "test", "tests", "vs", "and", "the", "for",
    "with", "about", "related", "information", "guide", "how", "what",
}
_DOWN_UNTIL = 0.0


def reset_cooldown() -> None:
    global _DOWN_UNTIL
    _DOWN_UNTIL = 0.0


def cool_down(seconds: float = COOLDOWN_S) -> None:
    """Stop asking the index for a while (used when a budget call times out)."""
    global _DOWN_UNTIL
    _DOWN_UNTIL = time.monotonic() + seconds


def _terms(query: str) -> list[str]:
    return [t for t in dict.fromkeys(query_terms(query))
            if len(t) >= 4 and t not in _GENERIC][:MAX_TERMS]


async def search(query: str, fetcher, domains=None, index: str | None = None,
                 max_domains: int = 3, max_results: int = 20) -> list[Candidate]:
    """Return candidates from index URLs on `domains`; never raises."""
    global _DOWN_UNTIL
    if time.monotonic() < _DOWN_UNTIL:
        return []
    terms = _terms(query)
    if not terms:
        return []
    index = index or DEFAULT_INDEX
    domains = list(domains or DEFAULT_DOMAINS)[:max_domains]

    found: list[Candidate] = []
    seen: set[str] = set()
    for domain in domains:
        for term in terms:
            url = f"{index}?url={quote(domain + '/*' + term + '*', safe='')}&output=json&limit=20"
            try:
                res = await fetcher.get(url, timeout=TIMEOUT_S)
            except Exception as exc:  # transport-level failure: index is best-effort
                log.warning("open-web index error", extra={"error": repr(exc)[:120]})
                _DOWN_UNTIL = time.monotonic() + COOLDOWN_S
                return found
            if not res.ok:
                if res.status in (502, 503, 504, "ERR"):
                    log.warning("open-web index unavailable",
                                extra={"status": str(res.status)})
                    _DOWN_UNTIL = time.monotonic() + COOLDOWN_S
                return found
            for line in (res.text or "").splitlines():
                line = line.strip()
                if not line.startswith("{"):
                    continue
                try:
                    record = json.loads(line)
                except ValueError:
                    continue
                target = (record.get("url") or "").strip()
                if not target.startswith("http") or target in seen:
                    continue
                seen.add(target)
                found.append(Candidate(
                    url=target,
                    title=unslug(target),
                    snippet=f"{domain} {fold(term)}",
                    published=record.get("timestamp"),
                    source="openweb",
                    method="cdx",
                ))
    ranked = score_candidates(query, found, max_results=max_results)
    for cand in ranked:
        cand.score = round(cand.score * 0.8, 4)  # below any registry match
    return ranked
