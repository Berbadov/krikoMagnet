"""Local SQLite cache keyed by (url_hash, query_hash) -> extracted markdown."""
import os
import sqlite3
import time

HERE = os.path.dirname(os.path.abspath(__file__))
DB_PATH = os.path.join(HERE, "..", "data", "pipeline_cache.sqlite")
QUERY_TTL = 6 * 3600
CONTENT_TTL = 7 * 24 * 3600


class Cache:
    def __init__(self, path=DB_PATH, query_ttl=QUERY_TTL, content_ttl=CONTENT_TTL):
        os.makedirs(os.path.dirname(path), exist_ok=True)
        self.query_ttl = query_ttl
        self.content_ttl = content_ttl
        self.conn = sqlite3.connect(path, check_same_thread=False, timeout=30)
        self.conn.execute("PRAGMA journal_mode=WAL")
        self.conn.execute("PRAGMA busy_timeout=30000")
        self.conn.execute(
            """CREATE TABLE IF NOT EXISTS cache(
                   url_hash TEXT, query_hash TEXT, url TEXT, domain TEXT,
                   markdown TEXT, status TEXT, fetched_at REAL,
                   PRIMARY KEY(url_hash, query_hash))"""
        )
        self.conn.execute(
            """CREATE TABLE IF NOT EXISTS qcache(
                   query_hash TEXT PRIMARY KEY, payload TEXT, cached_at REAL)"""
        )
        self.conn.commit()

    def get_query(self, query_hash, ttl=None):
        ttl = self.query_ttl if ttl is None else ttl
        cur = self.conn.execute(
            "SELECT payload, cached_at FROM qcache WHERE query_hash=?", (query_hash,)
        )
        row = cur.fetchone()
        if row is None:
            return None
        if ttl >= 0 and time.time() - (row[1] or 0) > ttl:
            return None
        return row[0]

    def set_query(self, query_hash, payload):
        self.conn.execute(
            "INSERT OR REPLACE INTO qcache(query_hash, payload, cached_at) VALUES (?,?,?)",
            (query_hash, payload, _now()),
        )
        self.conn.commit()

    def get(self, url_hash, query_hash):
        cur = self.conn.execute(
            "SELECT markdown, status, url, domain, fetched_at FROM cache "
            "WHERE url_hash=? AND query_hash=?",
            (url_hash, query_hash),
        )
        row = cur.fetchone()
        if row is None:
            return None
        if self.content_ttl >= 0 and time.time() - (row[4] or 0) > self.content_ttl:
            return None
        return {"markdown": row[0], "status": row[1], "url": row[2], "domain": row[3]}

    def set(self, url_hash, query_hash, url, domain, markdown, status):
        self.conn.execute(
            """INSERT OR REPLACE INTO cache(url_hash, query_hash, url, domain, markdown, status, fetched_at)
               VALUES (?,?,?,?,?,?,?)""",
            (url_hash, query_hash, url, domain, markdown, status, _now()),
        )
        self.conn.commit()

    def invalidate(self):
        """Drop cached answers (keeps extracted content)."""
        with self.conn:
            self.conn.execute("DELETE FROM qcache")

    def prune(self, now=None):
        """Delete expired query answers and content."""
        now = _now() if now is None else now
        with self.conn:
            if self.query_ttl >= 0:
                self.conn.execute("DELETE FROM qcache WHERE cached_at < ?",
                                  (now - self.query_ttl,))
            if self.content_ttl >= 0:
                self.conn.execute("DELETE FROM cache WHERE fetched_at < ?",
                                  (now - self.content_ttl,))

    def close(self):
        self.conn.close()


def _now():
    return time.time()


def sha16(s: str) -> str:
    import hashlib
    return hashlib.sha256(s.encode("utf-8")).hexdigest()[:16]
