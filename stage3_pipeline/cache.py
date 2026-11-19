"""Local SQLite cache keyed by (url_hash, query_hash) -> extracted markdown."""
import os
import sqlite3

HERE = os.path.dirname(os.path.abspath(__file__))
DB_PATH = os.path.join(HERE, "..", "data", "pipeline_cache.sqlite")


class Cache:
    def __init__(self, path=DB_PATH):
        os.makedirs(os.path.dirname(path), exist_ok=True)
        self.conn = sqlite3.connect(path, check_same_thread=False)
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

    def get_query(self, query_hash):
        cur = self.conn.execute("SELECT payload FROM qcache WHERE query_hash=?", (query_hash,))
        row = cur.fetchone()
        return row[0] if row else None

    def set_query(self, query_hash, payload):
        self.conn.execute(
            "INSERT OR REPLACE INTO qcache(query_hash, payload, cached_at) VALUES (?,?,?)",
            (query_hash, payload, _now()),
        )
        self.conn.commit()

    def get(self, url_hash, query_hash):
        cur = self.conn.execute(
            "SELECT markdown, status, url, domain FROM cache WHERE url_hash=? AND query_hash=?",
            (url_hash, query_hash),
        )
        row = cur.fetchone()
        if row is None:
            return None
        return {"markdown": row[0], "status": row[1], "url": row[2], "domain": row[3]}

    def set(self, url_hash, query_hash, url, domain, markdown, status):
        self.conn.execute(
            """INSERT OR REPLACE INTO cache(url_hash, query_hash, url, domain, markdown, status, fetched_at)
               VALUES (?,?,?,?,?,?,?)""",
            (url_hash, query_hash, url, domain, markdown, status, _now()),
        )
        self.conn.commit()

    def close(self):
        self.conn.close()


def _now():
    import time
    return time.time()


def sha16(s: str) -> str:
    import hashlib
    return hashlib.sha256(s.encode("utf-8")).hexdigest()[:16]
