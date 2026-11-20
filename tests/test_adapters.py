import asyncio
import os

from fetchlib import FetchResult
from sites.adapters import (
    Candidate,
    clear_sitemap_memory,
    enforce_brand_terms,
    enforce_intent_terms,
    enforce_model_terms,
    enforce_product_terms,
    parse_xml,
    run_rss,
    run_search,
    run_sitemap,
    score_candidates,
)
from sites.text import query_terms, stem_token

FIX = os.path.join(os.path.dirname(__file__), "fixtures")


def read(name):
    with open(os.path.join(FIX, name), encoding="utf-8") as f:
        return f.read()


class FixtureFetcher:
    def __init__(self, pages):
        self.pages = pages

    async def get(self, url, **kwargs):
        text = self.pages.get(url, "")
        return FetchResult(
            url=url,
            final_url=url,
            status=200 if text else 404,
            body=text.encode("utf-8"),
            text=text,
            headers={"content-type": "text/html; charset=utf-8"} if text else {},
        )


def test_parse_search_results():
    site = {"id": "fixture", "domain": "example.com", "name": "Fixture"}
    method = {
        "url": "https://example.com/search?q={query}",
        "link_selector": 'a[href^="/recalls/"]',
    }
    fetcher = FixtureFetcher({"https://example.com/search?q=anker": read("search_results.html")})

    async def run():
        return await run_search(site, method, "anker", fetcher)

    outcome = asyncio.run(run())
    assert outcome.status == "ok"
    urls = [c.url for c in outcome.candidates]
    assert "https://example.com/recalls/2025/anker-power-banks" in urls
    assert len(urls) == 2


def test_parse_sitemap_index_and_child():
    site = {"id": "fixture", "domain": "example.com", "name": "Fixture"}
    method = {
        "urls": ["https://example.com/sitemap.xml"],
        "include_children": ["sitemap-guides"],
        "max_sitemaps": 2,
    }
    fetcher = FixtureFetcher({
        "https://example.com/sitemap.xml": read("sitemap_index.xml"),
        "https://example.com/sitemap-guides.xml": read("sitemap_guides.xml"),
    })
    clear_sitemap_memory()

    async def run():
        return await run_sitemap(site, method, "iphone 15 screen", fetcher)

    outcome = asyncio.run(run())
    assert outcome.status == "ok"
    assert outcome.candidates[0].url.endswith("/guide/iphone-15-screen-replacement")
    assert all("oled-tv" not in c.url for c in outcome.candidates)


def test_parse_rss_and_match_turkish_query():
    site = {"id": "gubis", "domain": "example.gov", "name": "GUBIS"}
    method = {"url": "https://example.gov/feed.xml", "max_results": 5}
    fetcher = FixtureFetcher({"https://example.gov/feed.xml": read("feed.xml")})
    clear_sitemap_memory()

    async def run():
        return await run_rss(site, method, "çamaşır suyu güvensiz ürün", fetcher)

    outcome = asyncio.run(run())
    assert outcome.status == "ok"
    assert outcome.candidates[0].url == "https://example.gov/notice/20983"
    assert "Detergents" in outcome.candidates[0].title


def test_score_requires_two_specific_terms():
    candidates = [
        Candidate(url="https://example.com/iphone-15-screen", title="iPhone 15 screen repair"),
        Candidate(url="https://example.com/iphone-15-camera", title="iPhone 15 camera"),
        Candidate(url="https://example.com/cartoon", title="cartoon cizgi film"),
    ]
    ranked = score_candidates("iphone 15 ekran çizgi", candidates, max_results=5)
    assert ranked
    assert all("cartoon" not in c.url for c in ranked)


def test_model_and_brand_filters():
    candidates = [
        Candidate(url="https://www.rtings.com/tv/reviews/lg/c1-oled", title="tv reviews lg c1 oled"),
        Candidate(url="https://www.rtings.com/tv/learn/burn-in", title="tv learn oled burn in"),
        Candidate(url="https://www.rtings.com/running-shoes/reviews/mount-to-coast/c1", title="running shoes c1"),
    ]
    model = enforce_model_terms("lg c1 burn in", candidates)
    brand = enforce_brand_terms("lg c1 burn in", model)
    assert len(model) == 1
    assert [c.url for c in brand] == ["https://www.rtings.com/tv/reviews/lg/c1-oled"]


def test_missing_model_falls_back_to_brand():
    candidates = [
        Candidate(url="https://www.cpsc.gov/Recalls/2025/Anker-Power-Banks", title="Anker Power Banks Recalled"),
        Candidate(url="https://www.ifixit.com/Guide/Lower-Case-Recall", title="Lower case recall replacement"),
    ]
    model = enforce_model_terms("anker a1257 recall", candidates)
    assert len(model) == 2
    brand = enforce_brand_terms("anker a1257 recall", model)
    assert [c.url for c in brand] == ["https://www.cpsc.gov/Recalls/2025/Anker-Power-Banks"]


def test_parse_xml_kinds():
    kind, entries = parse_xml(read("sitemap_index.xml"))
    assert kind == "sitemapindex" and len(entries) == 1
    kind, entries = parse_xml(read("feed.xml"))
    assert kind == "rss" and len(entries) == 2


def test_model_compaction_and_turkish_stemming():
    terms = query_terms("samsung galaxy note 7 recall")
    assert "note7" in terms
    assert stem_token("filtresi") == "filtre"
    assert stem_token("arizasi") == "ariza"
    assert stem_token("recalled") == "recall"


def test_product_and_intent_filters():
    candidates = [
        Candidate(url="https://example.com/whirlpool-dishwashers", title="Whirlpool dishwashers"),
        Candidate(url="https://example.com/whirlpool-refrigerator-drain", title="Whirlpool refrigerator drain"),
        Candidate(url="https://example.com/lg-washing-machine-review", title="LG washing machine review"),
    ]
    product = enforce_product_terms("whirlpool dishwasher not draining", candidates)
    assert [c.url for c in product] == ["https://example.com/whirlpool-dishwashers"]

    intent = enforce_intent_terms("lg washing machine review", candidates)
    assert [c.url for c in intent] == ["https://example.com/lg-washing-machine-review"]
