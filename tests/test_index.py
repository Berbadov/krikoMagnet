import asyncio
import os

from fetchlib import FetchResult
from sites.index import SiteIndex, _rows_from_entries, ingest_site

FIX = os.path.join(os.path.dirname(__file__), "fixtures")


def read(name):
    with open(os.path.join(FIX, name), encoding="utf-8") as f:
        return f.read()


class FixtureFetcher:
    def __init__(self, pages):
        self.pages = pages
        self.calls = []
        self.not_modified = set()

    async def get(self, url, **kwargs):
        self.calls.append(url)
        text = self.pages.get(url, "")
        return FetchResult(
            url=url,
            final_url=url,
            status=200 if text else 404,
            body=text.encode("utf-8"),
            text=text,
            headers={"content-type": "application/xml"} if text else {},
            not_modified=url in self.not_modified,
        )


SITE = {"id": "fixture", "domain": "example.com", "name": "Fixture",
        "methods": [{"type": "sitemap", "url": "https://example.com/sitemap.xml",
                     "include_children": ["guides"], "max_results": 5}]}


def test_rows_from_urlset_and_turkish_query(tmp_path):
    index = SiteIndex(str(tmp_path / "index.sqlite"))
    try:
        rows = _rows_from_entries(
            SITE,
            [
                {"loc": "https://example.com/vaillant-kombi-ariza-kodu-f28"},
                {"loc": "https://example.com/unrelated-page"},
                {"loc": "https://example.com/logo.png"},
            ],
            "urlset",
        )
        assert len(rows) == 2
        assert index.replace_site("fixture", rows, full=True) == 2
        hits = index.query(["fixture"], "vaillant kombi arıza kodu")
        assert [c.url for c in hits] == ["https://example.com/vaillant-kombi-ariza-kodu-f28"]
        assert index.query(["fixture"], "zzz yok") == []
    finally:
        index.close()


def test_ingest_sitemap_index_into_index(tmp_path):
    site = dict(SITE)
    fetcher = FixtureFetcher({
        "https://example.com/sitemap.xml": read("sitemap_index.xml"),
        "https://example.com/sitemap-guides.xml": read("sitemap_guides.xml"),
    })
    index = SiteIndex(str(tmp_path / "index.sqlite"))

    async def run():
        return await ingest_site(site, fetcher, index, full=True)

    try:
        result = asyncio.run(run())
        assert result["pages"] >= 1
        assert index.site_pages("fixture") >= 1
        hits = index.query(["fixture"], "iphone 15 screen replacement")
        assert hits and hits[0].url.endswith("/guide/iphone-15-screen-replacement")
    finally:
        index.close()


def test_full_rebuild_replaces_and_refresh_upserts(tmp_path):
    index = SiteIndex(str(tmp_path / "index.sqlite"))
    try:
        index.replace_site("fixture", _rows_from_entries(
            SITE, [{"loc": "https://example.com/old-page"}], "urlset"), full=True)
        index.replace_site("fixture", _rows_from_entries(
            SITE, [{"loc": "https://example.com/new-page"}], "urlset"), full=True)
        assert index.site_pages("fixture") == 1
        assert index.query(["fixture"], "old page") == []
        index.replace_site("fixture", _rows_from_entries(
            SITE, [{"loc": "https://example.com/third-page"}], "urlset"), full=False)
        assert index.site_pages("fixture") == 2
        empty = index.replace_site("fixture", [], full=True)
        assert empty == 0 and index.site_pages("fixture") == 2
    finally:
        index.close()
