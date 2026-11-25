import asyncio

from fetchlib import FetchResult
from sites.adapters import run_json_search, run_rss
from sites.planner import classify, plan_sites
from sites.registry import load_registry, load_tranco_ranks

WIKI_JSON = """
{"pages": [
  {"id": 1635, "key": "Kolmogorov_complexity", "title": "Kolmogorov complexity",
   "excerpt": "the <span class=\\"searchmatch\\">Kolmogorov</span> complexity of an object is the length of a shortest program",
   "description": "Measure of algorithmic complexity"},
  {"id": 91161, "key": "Algorithmic_information_theory", "title": "Algorithmic information theory",
   "excerpt": "Algorithmic information theory studies Kolmogorov complexity", "description": null}
]}
"""

ATOM = """<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>query results</title>
  <entry>
    <id>http://arxiv.org/abs/2401.00001v1</id>
    <title>Kolmogorov complexity bounds</title>
    <summary>We bound Kolmogorov complexity for finite strings.</summary>
    <published>2024-01-02T03:04:05Z</published>
    <link rel="alternate" href="http://arxiv.org/abs/2401.00001v1"/>
  </entry>
</feed>
"""


class JsonFetcher:
    def __init__(self, pages):
        self.pages = pages
        self.seen = []

    async def get(self, url, **kwargs):
        self.seen.append(url)
        body = ""
        for prefix, value in self.pages.items():
            if url.startswith(prefix):
                body = value
                break
        return FetchResult(url=url, final_url=url, status=200 if body else 404,
                           body=body.encode("utf-8"), text=body,
                           headers={"content-type": "application/json" if body else ""})


def test_wikipedia_json_search_maps_fields():
    site = {"id": "wikipedia", "domain": "en.wikipedia.org", "name": "Wikipedia"}
    method = {
        "type": "json_search",
        "url": "https://api.example.test/search?q={query}&limit=10",
        "items_path": "pages",
        "title": "title",
        "snippet": "excerpt",
        "url_field": "key",
        "url_template": "https://en.wikipedia.org/wiki/{value}",
        "probes": 1,
        "max_results": 10,
    }
    fetcher = JsonFetcher({"https://api.example.test/search": WIKI_JSON})

    async def run():
        return await run_json_search(site, method, "kolmogorov complexity", fetcher)

    outcome = asyncio.run(run())
    assert outcome.status == "ok"
    urls = [c.url for c in outcome.candidates]
    assert "https://en.wikipedia.org/wiki/Kolmogorov_complexity" in urls
    first = outcome.candidates[0]
    assert first.title == "Kolmogorov complexity"
    assert "<span" not in first.snippet
    assert first.method == "json_search"


def test_atom_feed_is_parsed_and_queried():
    site = {"id": "arxiv", "domain": "arxiv.org", "name": "arXiv"}
    method = {
        "type": "rss",
        "url": "https://api.example.test/query?search_query={query}",
        "max_results": 5,
    }
    fetcher = JsonFetcher({"https://api.example.test/query": ATOM})

    async def run():
        return await run_rss(site, method, "kolmogorov complexity", fetcher)

    outcome = asyncio.run(run())
    assert outcome.status == "ok"
    assert fetcher.seen and fetcher.seen[0] == "https://api.example.test/query?search_query=kolmogorov+complexity"
    assert outcome.candidates[0].url == "http://arxiv.org/abs/2401.00001v1"
    assert outcome.candidates[0].published.startswith("2024-01-02")


def test_unknown_topic_falls_back_to_general_sources():
    reg = load_registry()
    ranks = load_tranco_ranks()
    assert classify("kolmogorov complexity and related information")["topics"] == []

    plan = plan_sites("kolmogorov complexity and related information", reg, ranks)
    assert plan["fallback"] == "general"
    assert plan["topics"] == ["general"]
    assert "wikipedia" in plan["sites"]
    assert "gsmarena" not in plan["sites"]


def test_consumer_query_keeps_the_registry():
    reg = load_registry()
    ranks = load_tranco_ranks()
    plan = plan_sites("lg c1 burn in", reg, ranks)
    assert plan["fallback"] is None
    assert "general" not in plan["topics"]
    assert not {"wikipedia", "crossref", "mathstackexchange"} & set(plan["sites"])
