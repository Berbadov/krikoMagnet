import os

import pytest

from ranker import Ranker

BIN = os.path.join(os.path.dirname(__file__), "..", "stage1_rs", "target", "release",
                   "kriko-magnet-ranker.exe")
pytestmark = pytest.mark.skipif(not os.path.exists(BIN), reason="Rust ranker not built")


def records():
    return [
        {"id": "a", "title": "lg c1 oled review", "snippet": "burn in test",
         "domain": "rtings.com", "published": "2026-01-01T00:00:00Z", "discovery": 0.8},
        {"id": "b", "title": "random tv news", "snippet": "oled panels",
         "domain": "example.com", "published": "2026-01-01T00:00:00Z", "discovery": 0.1},
    ]


def test_resident_ranker_matches_one_shot():
    ranker = Ranker()
    try:
        resident = ranker.rank(records(), "lg c1 burn in")
        one_shot = ranker._one_shot(records(), "lg c1 burn in", None, True)
        assert [r["id"] for r in resident] == [r["id"] for r in one_shot]
        assert resident[0]["score"] == pytest.approx(one_shot[0]["score"])
        assert resident[0]["id"] == "a"
    finally:
        ranker.close()
