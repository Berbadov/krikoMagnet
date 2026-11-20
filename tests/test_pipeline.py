import os
import sys

ROOT = os.path.join(os.path.dirname(__file__), "..")
sys.path.insert(0, os.path.join(ROOT, "stage3_pipeline"))
sys.path.insert(0, ROOT)

from pipeline import Searcher
from sites.discovery import base_domain
from sites.text import ranker_query


class Cand:
    def __init__(self, url):
        self.url = url


def test_domain_diversity_cap_keeps_top_three_per_domain():
    searcher = Searcher.__new__(Searcher)
    searcher.per_domain_limit = 3
    urls = [
        "https://a.example/1", "https://a.example/2", "https://a.example/3",
        "https://a.example/4", "https://b.example/5", "https://c.example/6",
    ]
    orig = {f"c{i}": Cand(u) for i, u in enumerate(urls)}
    ranked = [{"id": f"c{i}", "score": 1.0 - i / 10} for i in range(len(urls))]
    chosen = searcher._diversify(ranked, orig, 5, base_domain)
    ids = [x["id"] for x in chosen]
    assert len(ids) == 5
    assert sum(1 for i in ids if orig[i].url.startswith("https://a.")) == 3
    assert "c4" in ids and "c5" in ids


def test_ranker_query_expands_turkish_product_aliases():
    text = ranker_query("samsung buzdolabı sorunu")
    assert "refrigerator" in text
    assert "boil" in ranker_query("vaillant kombi arıza")
