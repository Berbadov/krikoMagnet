import asyncio
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "stage3_pipeline"))

from fetchlib import FetchResult
from worker import fetch_many


class SlowFetcher:
    def __init__(self):
        self.closed = False

    async def get(self, url, **kwargs):
        if "slow" in url:
            await asyncio.sleep(5)
        return FetchResult(url=url, status=200, body=b"<html><body>ok</body></html>",
                           text="<html><body>ok</body></html>",
                           headers={"content-type": "text/html"})

    async def close(self):
        self.closed = True


def test_fetch_budget_returns_partial_results():
    fetcher = SlowFetcher()

    async def run():
        return await fetch_many(["https://a.example/fast", "https://b.example/slow"],
                                concurrency=2, fetcher=fetcher, budget_ms=100)

    results = asyncio.run(run())
    assert "https://a.example/fast" in results
    assert "https://b.example/slow" not in results
