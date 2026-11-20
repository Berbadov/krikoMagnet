"""Fan out a query to the selected site adapters and merge candidates."""
from __future__ import annotations

import asyncio
import re
from datetime import datetime
from urllib.parse import urlsplit, urlunsplit

from .adapters import (
    Candidate, enforce_brand_terms, enforce_intent_terms, enforce_model_terms,
    enforce_product_terms, run_site, score_candidates,
)
from .planner import plan_sites
from .registry import load_registry, load_tranco_ranks
from .text import long_ago, ranker_text, unslug

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
                   max_sites: int = 6, max_candidates: int = 160) -> dict:
    registry = registry or load_registry()
    ranks = load_tranco_ranks()
    plan = plan_sites(query, registry, ranks, max_sites=max_sites)
    selected = plan["selected"]

    if fetcher is None:
        from fetchlib import Fetcher
        own = Fetcher()
        try:
            outcomes = await _run_all(selected, query, own)
        finally:
            await own.close()
    else:
        outcomes = await _run_all(selected, query, fetcher)

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


async def _run_all(selected: list[dict], query: str, fetcher) -> list:
    results = await asyncio.gather(
        *[run_site(site, query, fetcher) for site in selected],
        return_exceptions=True,
    )
    outcomes = []
    from .adapters import AdapterOutcome
    for site, result in zip(selected, results):
        if isinstance(result, Exception):
            outcomes.append(AdapterOutcome(site["id"], "?", "error", [], 0, 0.0, repr(result)[:160]))
        else:
            outcomes.extend(result)
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
