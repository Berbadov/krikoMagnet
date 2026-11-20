import asyncio

from fetchlib import FetchResult
from sites import openweb


class FakeFetcher:
    def __init__(self, status=200, body=""):
        self.status = status
        self.body = body
        self.seen = []

    async def get(self, url, **kwargs):
        self.seen.append(url)
        return FetchResult(url=url, final_url=url, status=self.status,
                           body=self.body.encode("utf-8"), text=self.body, headers={})


def setup_function(module):
    openweb.reset_cooldown()


def test_cdx_records_become_candidates():
    body = "\n".join([
        '{"url": "https://en.wikipedia.org/wiki/Kolmogorov_complexity", "timestamp": "20261101000000"}',
        '{"url": "https://en.wikipedia.org/wiki/Kolmogorov_space", "timestamp": "20261001000000"}',
    ])
    fetcher = FakeFetcher(body=body)

    async def run():
        return await openweb.search("kolmogorov complexity", fetcher,
                                    domains=["en.wikipedia.org"], max_domains=1)

    candidates = asyncio.run(run())
    urls = [c.url for c in candidates]
    assert "https://en.wikipedia.org/wiki/Kolmogorov_complexity" in urls
    assert all(c.source == "openweb" and c.method == "cdx" for c in candidates)
    assert len(fetcher.seen) == 2  # one index query per distinctive query term
    assert all("index.commoncrawl.org" in u for u in fetcher.seen)
    assert "kolmogorov" in fetcher.seen[0].lower()


def test_dead_index_cools_down_instead_of_retrying():
    fetcher = FakeFetcher(status=504, body="<html>504</html>")

    async def run_twice():
        first = await openweb.search("kolmogorov complexity", fetcher, max_domains=2)
        second = await openweb.search("kolmogorov complexity", fetcher, max_domains=2)
        return first, second

    first, second = asyncio.run(run_twice())
    assert first == [] and second == []
    assert len(fetcher.seen) == 1  # the second call short-circuits on cooldown


def test_generic_only_query_skips_the_index():
    fetcher = FakeFetcher(body="")

    async def run():
        return await openweb.search("best review information", fetcher)

    assert asyncio.run(run()) == []
    assert fetcher.seen == []
