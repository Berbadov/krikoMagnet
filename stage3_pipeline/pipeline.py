"""Kriko Magnet Stage 3: full-stack pipeline.

query -> query planner + site adapters (or curated fallback) -> Rust ranker
(Stage 1) -> top-K candidates -> async fetch + extract (Stage 2) -> SQLite
cache -> structured JSON results.
"""
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.join(HERE, "..")
sys.path.insert(0, ROOT)

from cache import Cache, sha16
from worker import fetch_many

RUST_BIN = os.path.join(ROOT, "stage1_rs", "target", "release", "kriko-magnet-ranker.exe")
CANDIDATES = os.path.join(HERE, "candidates.jsonl")
QUERIES_PIPE = os.path.join(HERE, "queries_pipeline.json")
TOP_K_DEFAULT = 3

class Searcher:
    def __init__(self, cache=None, top_k=TOP_K_DEFAULT, stats=None,
                 discovery="registry", registry=None, max_sites=6, fetcher=None,
                 index=None, use_index=True, index_path=None, ranker=None,
                 page_budget_ms=0):
        self.cache = cache or Cache()
        self.top_k = top_k
        self.page_budget_ms = page_budget_ms
        self.stats = stats or {"bytes": 0, "fetches": 0, "robots_skips": 0}
        self.discovery = discovery
        self.registry = registry
        self.max_sites = max_sites
        self.fetcher = fetcher
        self._owns_fetcher = fetcher is None
        self.index = index
        self._owns_index = False
        self.ranker = ranker
        self._owns_ranker = False
        if discovery == "curated":
            self.candidates = self._load_candidates()
        else:
            from sites import load_registry
            self.registry = registry or load_registry()
            self.candidates = None
            if self.index is None and use_index:
                from sites.index import SiteIndex
                self.index = SiteIndex(index_path) if index_path else SiteIndex()
                self._owns_index = True

    def _get_ranker(self):
        if self.ranker is None:
            from ranker import Ranker
            self.ranker = Ranker()
            self._owns_ranker = True
        return self.ranker

    def _load_candidates(self):
        m = {}
        with open(CANDIDATES, encoding="utf-8") as f:
            for line in f:
                rec = json.loads(line)
                m[rec["id"]] = rec
        return m

    async def _get_fetcher(self):
        if self.fetcher is None:
            from fetchlib import Fetcher
            self.fetcher = Fetcher()
        return self.fetcher

    async def close(self):
        if self._owns_fetcher and self.fetcher is not None:
            await self.fetcher.close()
            self.fetcher = None
        if self._owns_index and self.index is not None:
            self.index.close()
            self.index = None
        if self._owns_ranker and self.ranker is not None:
            self.ranker.close()
            self.ranker = None
        self.cache.close()

    def _rank_rust(self, corpus_recs, query_text, qid="q0"):
        ranked = self._get_ranker().rank(corpus_recs, query_text)
        return ranked or []

    async def _rank_registry(self, query_text, k):
        from sites import discover, to_corpus
        from sites.text import ranker_text

        fetcher = await self._get_fetcher()
        before = dict(fetcher.stats)
        disc = await discover(query_text, registry=self.registry, fetcher=fetcher,
                              max_sites=self.max_sites, index=self.index)
        candidates = disc["candidates"]
        if not candidates:
            return disc, [], {}

        corpus_recs = to_corpus(candidates)
        ranked = self._rank_rust(corpus_recs, ranker_text(query_text))

        by_id = {rec["id"]: rec for rec in corpus_recs}
        orig = {f"c{i:04d}": c for i, c in enumerate(candidates)}
        ranked = [x for x in ranked if x["id"] in by_id][:k]
        disc["fetches"] = fetcher.stats["requests"] - before["requests"]
        disc["bytes"] = fetcher.stats["bytes"] - before["bytes"]
        disc["robots_skips"] = fetcher.stats["robots_skips"] - before["robots_skips"]
        disc["sources_with_results"] = len({c.source for c in candidates})
        return disc, ranked, orig

    async def search(self, query_text, k=None):
        k = k or self.top_k
        t0 = time.perf_counter()
        qh = sha16(f"{self.discovery}|{k}|{query_text}")
        cached_query = self.cache.get_query(qh)
        if cached_query is not None:
            payload = json.loads(cached_query)
            elapsed = (time.perf_counter() - t0) * 1000
            for r in payload["results"]:
                r["cache_hit"] = True
                r["latency_ms"] = round(elapsed, 3)
            payload["elapsed_ms"] = round(elapsed, 3)
            return payload

        if self.discovery == "curated":
            return await self._search_curated(query_text, k, qh, t0)
        return await self._search_registry(query_text, k, qh, t0)

    async def _search_registry(self, query_text, k, qh, t0):
        stats_before = dict(self.stats)
        disc, ranked, orig = await self._rank_registry(query_text, k)
        results = []
        to_fetch = []
        pending = {}

        for item in ranked:
            cid = item["id"]
            cand = orig[cid]
            uh = sha16(cand.url)
            cached = self.cache.get(uh, qh)
            if cached is not None:
                results.append(self._result(cand, item["score"], cached["markdown"],
                                            cached["status"], t0, True, cid))
            else:
                to_fetch.append(cand.url)
                pending[cand.url] = (cand, item["score"], cid)

        if to_fetch:
            fetcher = await self._get_fetcher()
            fetched = await fetch_many(to_fetch, concurrency=max(4, k), stats=self.stats,
                                       fetcher=fetcher, budget_ms=self.page_budget_ms)
            for url, fr in fetched.items():
                cand, score, cid = pending[url]
                uh = sha16(url)
                self.cache.set(uh, qh, url, cand.source, fr["markdown"], str(fr["status"]))
                results.append(self._result(cand, score, fr["markdown"], fr["status"], t0, False, cid))

        order = {item["id"]: i for i, item in enumerate(ranked)}
        results.sort(key=lambda r: order.get(r.get("candidate_id"), 999))
        elapsed_ms = (time.perf_counter() - t0) * 1000
        plan = disc["plan"]
        page_fetches = self.stats["fetches"] - stats_before["fetches"]
        page_bytes = self.stats["bytes"] - stats_before["bytes"]
        payload = {
            "query": query_text,
            "elapsed_ms": round(elapsed_ms, 3),
            "results": results,
            "plan": {
                "language": plan["language"],
                "topics": plan["topics"],
                "sites": plan["sites"],
                "sources_with_results": disc.get("sources_with_results", 0),
            },
            "fetch_stats": {
                "discovery_fetches": disc.get("fetches", 0),
                "discovery_bytes": disc.get("bytes", 0),
                "page_fetches": page_fetches,
                "page_bytes": page_bytes,
                "total_fetches": disc.get("fetches", 0) + page_fetches,
                "total_bytes": disc.get("bytes", 0) + page_bytes,
                "robots_skips": disc.get("robots_skips", 0) + (self.stats["robots_skips"] - stats_before["robots_skips"]),
            },
        }
        self.cache.set_query(qh, json.dumps(payload, ensure_ascii=False))
        return payload

    def _result(self, cand, score, markdown, status, t0, cache_hit, candidate_id):
        from urllib.parse import urlparse
        return {
            "url": cand.url,
            "domain": urlparse(cand.url).netloc,
            "title": cand.title,
            "snippet": cand.snippet,
            "published": cand.published,
            "source": cand.source,
            "method": cand.method,
            "rank_score": score,
            "clean_markdown": markdown,
            "status": status,
            "latency_ms": round((time.perf_counter() - t0) * 1000, 3),
            "cache_hit": cache_hit,
            "candidate_id": candidate_id,
        }

    async def _search_curated(self, query_text, k, qh, t0):
        corpus_recs = list(self.candidates.values())
        ranked = self._rank_rust(corpus_recs, query_text)[:k]
        results = []
        to_fetch = []
        pending = {}
        for item in ranked:
            meta = self.candidates.get(item["id"])
            if not meta:
                continue
            uh = sha16(meta["url"])
            cached = self.cache.get(uh, qh)
            if cached is not None:
                results.append({
                    "url": cached["url"], "domain": cached["domain"],
                    "title": meta.get("title", ""), "snippet": meta.get("snippet", ""),
                    "published": meta.get("published"), "source": "curated", "method": "curated",
                    "rank_score": item["score"], "clean_markdown": cached["markdown"],
                    "status": cached["status"],
                    "latency_ms": round((time.perf_counter() - t0) * 1000, 3),
                    "cache_hit": True,
                })
            else:
                to_fetch.append(meta["url"])
                pending[meta["url"]] = (item["id"], item["score"], meta)

        if to_fetch:
            fetcher = await self._get_fetcher()
            fetched = await fetch_many(to_fetch, concurrency=4, stats=self.stats, fetcher=fetcher)
            for url, fr in fetched.items():
                cid, score, meta = pending[url]
                uh = sha16(url)
                self.cache.set(uh, qh, url, meta["domain"], fr["markdown"], str(fr["status"]))
                results.append({
                    "url": url, "domain": meta["domain"],
                    "title": meta.get("title", ""), "snippet": meta.get("snippet", ""),
                    "published": meta.get("published"), "source": "curated", "method": "curated",
                    "rank_score": score, "clean_markdown": fr["markdown"],
                    "status": fr["status"],
                    "latency_ms": round(fr["fetch_ms"] + fr.get("parse_ms", 0), 3),
                    "cache_hit": False,
                })

        order = {item["id"]: i for i, item in enumerate(ranked)}
        results.sort(key=lambda r: order.get(self._curated_id_for_url(r["url"]), 999))
        elapsed_ms = (time.perf_counter() - t0) * 1000
        payload = {"query": query_text, "elapsed_ms": round(elapsed_ms, 3), "results": results}
        self.cache.set_query(qh, json.dumps(payload, ensure_ascii=False))
        return payload

    def _curated_id_for_url(self, url):
        for cid, m in self.candidates.items():
            if m["url"] == url:
                return cid
        return None
