"""Fan out a query to the selected site adapters and merge candidates."""
from __future__ import annotations

import asyncio
import re
import time
from datetime import datetime
from urllib.parse import urlsplit, urlunsplit

from .adapters import (
    Candidate, enforce_brand_terms, enforce_intent_terms, enforce_model_terms,
    enforce_product_terms, run_site, score_candidates,
)
from .circuit import CircuitBreaker
from .log import get_logger
from .planner import plan_sites
from .registry import load_registry, load_tranco_ranks
from .text import long_ago, ranker_text, unslug

log = get_logger("sites.discovery")
BREAKER = CircuitBreaker()
SITE_TIMEOUT = 25.0

MULTI_SUFFIX = {
    "co.uk", "com.tr", "gov.tr", "org.tr", "net.tr", "com.au", "co.jp",
    "co.nz", "com.br", "co.in", "com.mx", "com.sg",
}
REVIEW_PAGE = re.compile(r"(-(?:handson-)?review-\d+)p\d+(\.php)$", re.I)
YEAR = re.compile(r"\b(19\d{2}|20\d{2})\b")


def _base_review_url(url: str) -> str:
    return REVIEW_PAGE.sub(r"\1\2", url)


def _latest_year(query: str, candidates: list[Candidate], topics: list[str]) -> list[Candidate]:
    if "cars" not in topics or YEAR.search(query):
        return candidates
    years = []
    for cand in candidates:
        found = [int(y) for y in YEAR.findall(f"{cand.url} {cand.title}")]
        if found:
            years.append(max(found))
    if not years:
        return candidates
    newest = min(max(years), datetime.now().year)
    if newest < 2020:
        return candidates
    if newest >= datetime.now().year - 1:
        kept = []
        for cand in candidates:
            found = [int(y) for y in YEAR.findall(f"{cand.url} {cand.title}")]
            if found and max(found) >= newest - 1:
                kept.append(cand)
        return kept or candidates
    kept = []
    for cand in candidates:
        found = [int(y) for y in YEAR.findall(f"{cand.url} {cand.title}")]
        if not found or max(found) >= newest - 1:
            kept.append(cand)
    return kept or candidates


def base_domain(url_or_host: str) -> str:
    host = urlsplit(url_or_host).netloc.lower() if "://" in url_or_host else url_or_host.lower()
    host = host.split(":")[0].lstrip(".")
    labels = host.split(".")
    if len(labels) <= 2:
        return host
    if ".".join(labels[-2:]) in MULTI_SUFFIX:
        return ".".join(labels[-3:])
    return ".".join(labels[-2:])


def _canonical(url: str) -> str:
    parts = urlsplit(url)
    path = parts.path or "/"
    if path != "/" and path.endswith("/"):
        path = path[:-1]
    return urlunsplit((parts.scheme.lower(), parts.netloc.lower(), path, parts.query, ""))


async def discover(query: str, registry: dict | None = None, fetcher=None,
                   max_sites: int = 6, max_candidates: int = 160,
                   index=None) -> dict:
    registry = registry or load_registry()
    ranks = load_tranco_ranks()
    plan = plan_sites(query, registry, ranks, max_sites=max_sites)
    selected = plan["selected"]

    if fetcher is None:
        from fetchlib import Fetcher
        own = Fetcher()
        try:
            outcomes = await _run_all(selected, query, own, index)
        finally:
            await own.close()
    else:
        outcomes = await _run_all(selected, query, fetcher, index)

    merged = {}
    for outcome in outcomes:
        for cand in outcome.candidates:
            base = _base_review_url(cand.url)
            if base != cand.url:
                cand.url = base
                cand.title = unslug(base)
                cand.snippet = f"{cand.source} {unslug(base)}"
            key = _canonical(cand.url)
            if key not in merged or cand.score > merged[key].score:
                merged[key] = cand
    candidates = score_candidates(query, list(merged.values()), max_results=max_candidates)
    candidates = enforce_model_terms(query, candidates)
    candidates = enforce_brand_terms(query, candidates)
    candidates = enforce_product_terms(query, candidates)
    candidates = enforce_intent_terms(query, candidates)
    candidates = _latest_year(query, candidates, plan["topics"])
    return {
        "query": query,
        "plan": plan,
        "outcomes": outcomes,
        "candidates": candidates,
    }


async def _run_all(selected: list[dict], query: str, fetcher, index=None) -> list:
    results = await asyncio.gather(
        *[_run_one(site, query, fetcher, index) for site in selected],
        return_exceptions=True,
    )
    outcomes = []
    from .adapters import AdapterOutcome
    for site, result in zip(selected, results):
        if isinstance(result, Exception):
            BREAKER.failure(site["id"])
            log.warning("adapter failed", extra={"site": site["id"], "error": repr(result)[:160]})
            outcomes.append(AdapterOutcome(site["id"], "?", "error", [], 0, 0.0, repr(result)[:160]))
            continue
        failed = (not any(o.candidates for o in result)
                  and any(o.status == "error" for o in result))
        if failed:
            BREAKER.failure(site["id"])
        elif result:
            BREAKER.success(site["id"])
        outcomes.extend(result)
    return outcomes


def _indexable(site: dict, index) -> bool:
    if index is None:
        return False
    if not any(m.get("type") in ("sitemap", "rss") for m in site["methods"]):
        return False
    try:
        return index.site_pages(site["id"]) > 0
    except Exception:
        return False


async def _run_one(site: dict, query: str, fetcher, index) -> list:
    """Serve sitemap/RSS sites from the local index; keep live search/API."""
    from .adapters import AdapterOutcome

    if not BREAKER.allow(site["id"]):
        log.warning("site circuit open", extra={"site": site["id"]})
        return [AdapterOutcome(site["id"], "-", "circuit-open", [], 0, 0.0,
                               "skipped: circuit open")]
    if _indexable(site, index):
        max_results = max(
            int(m.get("max_results", 40)) for m in site["methods"]
            if m.get("type") in ("sitemap", "rss")
        )
        candidates = index.query([site["id"]], query, max_results=max_results)
        return [AdapterOutcome(site["id"], "index",
                               "ok" if candidates else "no-results",
                               candidates, 0, 0.0)]
    t0 = time.monotonic()
    outcomes = await asyncio.wait_for(run_site(site, query, fetcher), timeout=SITE_TIMEOUT)
    log.debug("site adapter", extra={"site": site["id"], "status": outcomes[-1].status,
                                     "candidates": sum(len(o.candidates) for o in outcomes),
                                     "ms": round((time.monotonic() - t0) * 1000, 1)})
    return outcomes


def to_corpus(candidates: list[Candidate]) -> list[dict]:
    records = []
    for i, cand in enumerate(candidates):
        records.append({
            "id": f"c{i:04d}",
            "url": cand.url,
            "domain": base_domain(cand.url),
            "title": ranker_text(cand.title),
            "snippet": ranker_text(cand.snippet),
            "published": (cand.published if cand.method in ("api", "rss", "search") else long_ago(90))
                         or long_ago(),
            "discovery": cand.score,
            "source": cand.source,
            "method": cand.method,
            "discovery_score": cand.score,
        })
    return records
