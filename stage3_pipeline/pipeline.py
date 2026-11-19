"""Kriko Magnet Stage 3: minimal full-stack pipeline.

query -> Rust ranker (Stage 1) -> top-K candidates -> async fetch + extract
(Stage 2) -> SQLite cache -> structured JSON results.
"""
import asyncio
import json
import os
import subprocess
import tempfile
import time

from cache import Cache, sha16
from worker import fetch_many

HERE = os.path.dirname(os.path.abspath(__file__))
RUST_BIN = os.path.join(HERE, "..", "stage1_rs", "target", "release", "kriko-magnet-ranker.exe")
CANDIDATES = os.path.join(HERE, "candidates.jsonl")
QUERIES_PIPE = os.path.join(HERE, "queries_pipeline.json")
TRANCO = os.path.join(HERE, "..", "data", "tranco_top100k.txt")
TOP_K_DEFAULT = 3


class Searcher:
    def __init__(self, cache=None, top_k=TOP_K_DEFAULT, stats=None):
        self.cache = cache or Cache()
        self.top_k = top_k
        self.stats = stats or {"bytes": 0, "fetches": 0}
        self.candidates = self._load_candidates()

    def _load_candidates(self):
        m = {}
        with open(CANDIDATES, encoding="utf-8") as f:
            for line in f:
                rec = json.loads(line)
                m[rec["id"]] = rec
        return m

    def _rank_rust(self, query_text, qid="q0"):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8") as tf:
            json.dump([{"id": qid, "text": query_text}], tf)
            qpath = tf.name
        try:
            now = time.time()
            cp = subprocess.run(
                [RUST_BIN, "--corpus", CANDIDATES, "--queries", qpath,
                 "--tranco", TRANCO, "--now", str(now), "--scores"],
                capture_output=True, text=True, check=True,
            )
            data = json.loads(cp.stdout)
            return data.get(qid, [])
        finally:
            os.unlink(qpath)

    async def search(self, query_text, k=None):
        k = k or self.top_k
        t0 = time.perf_counter()
        qh = sha16(query_text)
        cached_query = self.cache.get_query(qh)
        if cached_query is not None:
            payload = json.loads(cached_query)
            elapsed = (time.perf_counter() - t0) * 1000
            for r in payload["results"]:
                r["cache_hit"] = True
                r["latency_ms"] = round(elapsed, 3)
            payload["elapsed_ms"] = round(elapsed, 3)
            return payload

        ranked = self._rank_rust(query_text)
        ranked = ranked[:k]
        results = []
        urls_to_fetch = []
        pending = {}

        for item in ranked:
            cid = item["id"]
            meta = self.candidates.get(cid)
            if not meta:
                continue
            url = meta["url"]
            qh, uh = sha16(query_text), sha16(url)
            cached = self.cache.get(uh, qh)
            if cached is not None:
                results.append({
                    "url": cached["url"], "domain": cached["domain"],
                    "rank_score": item["score"],
                    "clean_markdown": cached["markdown"],
                    "status": cached["status"],
                    "latency_ms": round((time.perf_counter() - t0) * 1000, 3),
                    "cache_hit": True,
                })
            else:
                urls_to_fetch.append(url)
                pending[url] = {"cid": cid, "score": item["score"], "qh": qh, "meta": meta}

        if urls_to_fetch:
            fetched = await fetch_many(urls_to_fetch, concurrency=4, stats=self.stats)
            for url, fr in fetched.items():
                p = pending[url]
                uh = sha16(url)
                self.cache.set(uh, p["qh"], url, p["meta"]["domain"], fr["markdown"], str(fr["status"]))
                results.append({
                    "url": url, "domain": p["meta"]["domain"],
                    "rank_score": p["score"],
                    "clean_markdown": fr["markdown"],
                    "status": fr["status"],
                    "latency_ms": round(fr["fetch_ms"] + fr.get("parse_ms", 0), 3),
                    "cache_hit": False,
                })

        # keep Rust order
        order = {item["id"]: i for i, item in enumerate(ranked)}
        results.sort(key=lambda r: order[self._cid_for_url(r["url"])] if self._cid_for_url(r["url"]) in order else 999)
        elapsed_ms = (time.perf_counter() - t0) * 1000
        payload = {"query": query_text, "elapsed_ms": round(elapsed_ms, 3), "results": results}
        self.cache.set_query(qh, json.dumps(payload, ensure_ascii=False))
        return payload

    def _cid_for_url(self, url):
        for cid, m in self.candidates.items():
            if m["url"] == url:
                return cid
        return None
