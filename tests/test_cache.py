import sys
import os

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "stage3_pipeline"))

from cache import Cache


def test_query_cache_ttl(tmp_path):
    cache = Cache(str(tmp_path / "c.sqlite"), query_ttl=60)
    try:
        cache.set_query("qh", "payload")
        assert cache.get_query("qh") == "payload"
        cache.conn.execute("UPDATE qcache SET cached_at=cached_at-120 WHERE query_hash='qh'")
        cache.conn.commit()
        assert cache.get_query("qh") is None
        cache.invalidate()
        assert cache.get_query("qh") is None
    finally:
        cache.close()


def test_content_cache_ttl_and_prune(tmp_path):
    cache = Cache(str(tmp_path / "c.sqlite"), content_ttl=60)
    try:
        cache.set("uh", "qh", "https://example.com/a", "example.com", "text", "200")
        assert cache.get("uh", "qh")["markdown"] == "text"
        cache.conn.execute("UPDATE cache SET fetched_at=fetched_at-120")
        cache.conn.commit()
        assert cache.get("uh", "qh") is None
        cache.prune()
        rows = cache.conn.execute("SELECT COUNT(*) FROM cache").fetchone()[0]
        assert rows == 0
    finally:
        cache.close()
