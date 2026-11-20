"""No-network engine for the MCP tests.

Wraps the real MagnetEngine (so result shaping, robot refusals and health are
the production code paths) with a stub searcher and a stub fetcher.
"""
from __future__ import annotations

import os
import tempfile
from types import SimpleNamespace

from fetchlib import FetchResult
from mcp_server.engine import MagnetEngine
from sites.adapters import Candidate

REGISTRY = {
    "sites": [
        {"id": "rtings", "domain": "rtings.com", "language": "en"},
        {"id": "gsmarena", "domain": "gsmarena.com", "language": "en"},
        {"id": "donanimhaber", "domain": "donanimhaber.com", "language": "tr"},
    ]
}

PAGE_HTML = (
    "<html><body><article><h1>LG C1 review</h1>"
    "<p>The LG C1 OLED showed visible burn-in after the torture test. "
    "SDR brightness held up, and the panel uniformity was good otherwise.</p>"
    "</article></body></html>"
)


class FakeFetcher:
    def __init__(self):
        self.stats = {"requests": 0, "bytes": 0, "cache_hits": 0,
                      "robots_skips": 0, "retries": 0}
        self.user_agent = "KrikoMagnet/0.2 (+https://kriko.cc/bot)"
        self.limiter = SimpleNamespace(min_interval=1.0)

    async def get(self, url, **kwargs):
        self.stats["requests"] += 1
        self.stats["bytes"] += len(PAGE_HTML)
        if url == "https://example.com/blocked":
            self.stats["robots_skips"] += 1
            return FetchResult(url=url, status="robots-denied", robots_skipped=True,
                               robots_allowed=False)
        if url == "https://example.com/page":
            return FetchResult(url=url, final_url=url, status=200, text=PAGE_HTML,
                               body=PAGE_HTML.encode(), robots_allowed=True,
                               headers={"content-type": "text/html; charset=utf-8"})
        return FetchResult(url=url, status=404, robots_allowed=True)


class FakeSearcher:
    async def search(self, query, k=None):
        rows = [
            {
                "url": "https://www.rtings.com/tv/reviews/lg/c1",
                "domain": "www.rtings.com",
                "title": "LG C1 OLED Review",
                "snippet": "burn-in test",
                "rank_score": 1.5,
                "clean_markdown": "Burn-in appeared after 200 hours. " * 40,
                "status": 200,
                "cache_hit": True,
                "source": "rtings",
            },
            {
                "url": "https://www.donanimhaber.com/lg-c1-oled",
                "domain": "www.donanimhaber.com",
                "title": "LG C1 OLED inceleme",
                "snippet": "yanma testi",
                "rank_score": 1.1,
                "clean_markdown": "Panel testinde yanma izi goruldu. " * 30,
                "status": 200,
                "cache_hit": False,
                "source": "donanimhaber",
            },
            {
                "url": "https://www.gsmarena.com/blocked",
                "domain": "www.gsmarena.com",
                "title": "Blocked page",
                "snippet": "",
                "rank_score": 0.4,
                "clean_markdown": "",
                "status": "robots",
                "cache_hit": False,
                "source": "gsmarena",
            },
            {
                "url": "https://www.macrumors.com/teapot",
                "domain": "www.macrumors.com",
                "title": "Teapot",
                "snippet": "",
                "rank_score": 0.2,
                "clean_markdown": "",
                "status": 403,
                "cache_hit": False,
                "source": "macrumors",
            },
        ][: k or 4]
        return {
            "query": query,
            "elapsed_ms": 12.5,
            "results": rows,
            "plan": {"language": "en", "topics": ["tv-audio"], "sites": ["rtings"]},
        }

    async def _rank_registry(self, query, k):
        candidates = [
            Candidate(url="https://www.rtings.com/tv/reviews/lg/c1",
                      title="LG C1 OLED Review", snippet="burn-in test",
                      source="rtings", method="index", score=1.5),
            Candidate(url="https://www.donanimhaber.com/lg-c1-oled",
                      title="LG C1 OLED inceleme", snippet="yanma testi",
                      source="donanimhaber", method="index", score=1.1),
        ]
        orig = {f"c{i:04d}": cand for i, cand in enumerate(candidates)}
        ranked = [{"id": key, "score": cand.score} for key, cand in orig.items()][:k]
        return {"plan": {"language": "en", "topics": ["tv-audio"], "sites": ["rtings"]}}, ranked, orig


def make_engine():
    index_path = os.path.join(tempfile.gettempdir(), "magnet-mcp-test", "index.sqlite")
    return MagnetEngine(fetcher=FakeFetcher(), searcher=FakeSearcher(),
                        registry=REGISTRY, index_path=index_path)
