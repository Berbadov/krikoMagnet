"""Tool logic behind the Kriko Magnet MCP server.

Everything heavy (curl_cffi, trafilatura, the site registry, the Rust ranker) is
imported on first use so the stdio handshake stays quick. The searcher, fetcher
and ranker are built once per process and reused.
"""
from __future__ import annotations

import asyncio
import os
import time
from urllib.parse import urlparse

from mcp.server.mcpserver.exceptions import ToolError

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RUST_BIN = os.path.join(ROOT, "stage1_rs", "target", "release", "kriko-magnet-ranker.exe")
DEFAULT_INDEX = os.path.join(ROOT, "data", "site_index.sqlite")

SNIPPET_CHARS = 280
TEXT_CHARS = 8000
MAX_K = 10
LANGS = ("en", "tr")
EMPTY_STATS = {"requests": 0, "bytes": 0, "cache_hits": 0, "robots_skips": 0, "retries": 0}


class EngineError(ToolError):
    """Bad tool arguments; surfaced to the client as a tool error."""


class MagnetEngine:
    def __init__(self, fetcher=None, searcher=None, registry=None, index_path=None):
        self._fetcher = fetcher
        self._searcher = searcher
        self._registry = registry
        self._index_path = index_path
        self._search_lock = asyncio.Lock()
        self._site_languages = None

    # -- lazy singletons ------------------------------------------------

    @property
    def registry(self):
        if self._registry is None:
            from sites import load_registry

            self._registry = load_registry()
        return self._registry

    async def _get_fetcher(self):
        if self._fetcher is None:
            from fetchlib import Fetcher

            self._fetcher = Fetcher()
        return self._fetcher

    async def _get_searcher(self):
        if self._searcher is None:
            from stage3_pipeline.pipeline import Searcher

            self._searcher = Searcher(
                fetcher=await self._get_fetcher(),
                registry=self.registry,
                index_path=self._index_path,
            )
        return self._searcher

    def _stats(self):
        return dict(self._fetcher.stats) if self._fetcher is not None else dict(EMPTY_STATS)

    def _polite(self, before):
        now = self._stats()
        delta = {key: now.get(key, 0) - before.get(key, 0) for key in EMPTY_STATS}
        return {
            "http_requests": delta["requests"],
            "bytes": delta["bytes"],
            "cache_hits": delta["cache_hits"],
            "robots_skips": delta["robots_skips"],
            "retries": delta["retries"],
            "min_host_interval_s": getattr(getattr(self._fetcher, "limiter", None),
                                           "min_interval", None),
            "user_agent": getattr(self._fetcher, "user_agent", None),
        }

    # -- tools ----------------------------------------------------------

    async def search(self, query, k=5, lang=None, fetch=True):
        query = (query or "").strip()
        if not query:
            raise EngineError("query must be a non-empty string")
        if isinstance(k, bool) or not isinstance(k, int) or not 1 <= k <= MAX_K:
            raise EngineError(f"k must be an integer from 1 to {MAX_K}")
        if lang is not None and lang not in LANGS:
            raise EngineError("lang must be 'en' or 'tr'")

        async with self._search_lock:
            before = self._stats()
            searcher = await self._get_searcher()
            if fetch:
                payload = await searcher.search(query, k=k)
                plan = payload.get("plan") or {}
                rows = [self._row(item, i + 1)
                        for i, item in enumerate(payload.get("results", []))]
                elapsed = payload.get("elapsed_ms")
            else:
                payload, rows = await self._rank_only(searcher, query, k)
                plan = payload.get("plan") or {}
                elapsed = payload.get("elapsed_ms")

            rows = self._filter_lang(rows, lang)
            results, skipped = [], []
            for row in rows:
                status = row.get("status")
                if status in ("robots", "robots-denied"):
                    skipped.append({"url": row["url"], "reason": "robots.txt disallows this path"})
                elif isinstance(status, int) and status >= 400:
                    skipped.append({"url": row["url"], "reason": f"HTTP {status}"})
                elif status == "ERR":
                    skipped.append({"url": row["url"], "reason": "fetch error"})
                else:
                    results.append(row)
            for i, row in enumerate(results):
                row["rank"] = i + 1

            return {
                "query": query,
                "k": k,
                "lang": lang,
                "pages_fetched": bool(fetch),
                "elapsed_ms": elapsed,
                "plan": {
                    "language": plan.get("language"),
                    "topics": plan.get("topics"),
                    "sites": plan.get("sites"),
                },
                "polite": self._polite(before),
                "results": results[:k],
                "skipped": skipped,
            }

    async def _rank_only(self, searcher, query, k):
        """Discovery + ranking without page fetches (reuses the Stage 3 path)."""
        t0 = time.perf_counter()
        disc, ranked, orig = await searcher._rank_registry(query, k)
        rows = []
        for i, item in enumerate(ranked):
            cand = orig.get(item["id"])
            if cand is None:
                continue
            rows.append(self._row({
                "url": cand.url,
                "title": cand.title,
                "snippet": cand.snippet,
                "rank_score": item.get("score", 0.0),
                "source": cand.source,
                "status": "not-fetched",
                "cache_hit": False,
            }, i + 1))
        return {
            "elapsed_ms": round((time.perf_counter() - t0) * 1000, 3),
            "plan": disc.get("plan") or {},
        }, rows

    def _row(self, item, rank):
        text = item.get("clean_markdown") or item.get("snippet") or ""
        url = item.get("url") or ""
        return {
            "rank": rank,
            "title": _clean(item.get("title"), 160),
            "url": url,
            "site": _base_domain(url),
            "snippet": _clean(text, SNIPPET_CHARS),
            "score": round(float(item.get("rank_score") or 0.0), 4),
            "cached": bool(item.get("cache_hit")),
            "fetched": item.get("status") == 200,
            "status": item.get("status"),
            "source": item.get("source"),
        }

    def _filter_lang(self, rows, lang):
        if not lang:
            return rows
        if self._site_languages is None:
            self._site_languages = {
                (site.get("domain") or "").lower(): site.get("language")
                for site in self.registry.get("sites", [])
            }
        wanted = [row for row in rows
                  if self._site_languages.get(_base_domain(row["url"])) == lang]
        return wanted or rows

    async def fetch(self, url):
        url = (url or "").strip()
        parsed = urlparse(url)
        if parsed.scheme not in ("http", "https") or not parsed.netloc:
            raise EngineError("url must be an absolute http(s) URL")

        fetcher = await self._get_fetcher()
        res = await fetcher.get(url)
        if res.robots_skipped:
            return {
                "url": url,
                "ok": False,
                "robots_allowed": False,
                "error": "robots.txt disallows this URL; not fetched",
            }
        if not res.ok:
            reason = res.error or f"HTTP {res.status}"
            return {"url": url, "ok": False, "robots_allowed": res.robots_allowed,
                    "status": res.status, "error": reason}

        from fetchlib import extract_text

        text = extract_text(res.text)
        if not text.strip():
            return {"url": url, "ok": False, "robots_allowed": True,
                    "status": res.status, "error": "no extractable main text"}
        return {
            "url": url,
            "final_url": res.final_url or url,
            "ok": True,
            "robots_allowed": True,
            "status": res.status,
            "cached": res.from_cache,
            "chars": len(text),
            "truncated": len(text) > TEXT_CHARS,
            "text": text[:TEXT_CHARS],
        }

    async def health(self, probe=False):
        from sites.discovery import BREAKER

        sites = self.registry.get("sites", [])
        report = {
            "ok": True,
            "sites": len(sites),
            "by_language": _counts(site.get("language") for site in sites),
            "ranker": {
                "binary": os.path.exists(RUST_BIN),
                "resident": self._ranker_running(),
            },
            "index": self._index_health(),
            "circuit_open": sorted(name for name, state in BREAKER.snapshot().items()
                                   if state.get("open")),
            "fetcher": self._stats() if self._fetcher is not None else None,
        }
        if probe:
            from sites.healthcheck import run

            rows = await run(test_extract=False)
            report["probe"] = [
                {"site": row["site"], "status": row["status"], "links": row["links"],
                 "circuit_open": row["circuit_open"], "secs": row["secs"]}
                for row in rows
            ]
            report["ok"] = all(row["links"] for row in rows)
        return report

    def _ranker_running(self):
        ranker = getattr(self._searcher, "ranker", None) if self._searcher is not None else None
        proc = getattr(ranker, "_proc", None)
        return bool(proc is not None and proc.poll() is None)

    def _index_health(self):
        path = self._index_path or DEFAULT_INDEX
        if not os.path.exists(path):
            return {"path": path, "pages": 0, "missing": True}
        try:
            from sites.index import SiteIndex

            index = SiteIndex(path)
            try:
                stats = index.stats().as_dict()
            finally:
                index.close()
        except Exception as exc:
            return {"path": path, "error": repr(exc)[:160]}
        stats["path"] = path
        stats.pop("per_site", None)
        return stats

    async def aclose(self):
        if self._searcher is not None:
            close = getattr(self._searcher, "close", None)
            if close is not None:
                await close()
        if self._fetcher is not None:
            close = getattr(self._fetcher, "close", None)
            if close is not None:
                await close()


def _clean(text, limit):
    value = " ".join((text or "").split())
    return value if len(value) <= limit else value[: limit - 1].rstrip() + "…"


_BASE_DOMAIN = None


def _base_domain(url):
    global _BASE_DOMAIN
    if _BASE_DOMAIN is None:
        from sites import base_domain

        _BASE_DOMAIN = base_domain
    return _BASE_DOMAIN(url)


def _counts(values):
    out = {}
    for value in values:
        if value:
            out[value] = out.get(value, 0) + 1
    return out
