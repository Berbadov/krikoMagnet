"""Persistent local index of sitemap/RSS page metadata (SQLite FTS5).

The index answers discovery locally, so a query fetches only the top-k pages
instead of every site's sitemap. It is filled by a separate, polite ingest
(``python -m sites.index build``) that uses the shared fetch layer: robots.txt,
honest UA, >=1 s/host spacing, conditional GETs on refresh. The query path
never bypasses robots.txt for the pages it then fetches.

Rows keep url/title/snippet/lastmod plus a folded, stemmed match text so the
same lexical scorer (``score_candidates``) runs over indexed and live
candidates.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import sqlite3
import sys
import time
from dataclasses import dataclass
from datetime import datetime
from urllib.parse import urlparse

from .adapters import (
    ASSET_EXT,
    MODEL_TOKEN,
    AdapterOutcome,
    Candidate,
    parse_xml,
    score_candidates,
)
from .registry import load_registry
from .text import iso_date, query_terms, ranker_text, stem_token, unslug

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_DB = os.path.join(HERE, "..", "data", "site_index.sqlite")
SCHEMA = """
CREATE TABLE IF NOT EXISTS pages(
    rowid INTEGER PRIMARY KEY, url TEXT UNIQUE NOT NULL, site TEXT NOT NULL,
    title TEXT, snippet TEXT, lastmod TEXT, method TEXT, text TEXT NOT NULL,
    updated REAL);
CREATE TABLE IF NOT EXISTS feeds(
    url TEXT PRIMARY KEY, site TEXT, kind TEXT, etag TEXT, lastmod TEXT,
    entries INTEGER, fetched_at REAL);
CREATE TABLE IF NOT EXISTS index_meta(key TEXT PRIMARY KEY, value TEXT);
CREATE VIRTUAL TABLE IF NOT EXISTS pages_fts
    USING fts5(url UNINDEXED, site, text, tokenize='unicode61');
CREATE INDEX IF NOT EXISTS pages_site ON pages(site);
"""
SCHEMA_VERSION = "2"


def _migrate(conn) -> None:
    """Rebuild the index when the FTS layout changes (site must be indexed)."""
    row = conn.execute(
        "SELECT value FROM index_meta WHERE key='schema'").fetchone()
    if row is not None and row[0] == SCHEMA_VERSION:
        return
    cols = {r[1] for r in conn.execute("PRAGMA table_info(pages)")}
    if cols and "rowid" not in cols:
        conn.executescript("DROP TABLE IF EXISTS pages_fts; DROP TABLE IF EXISTS pages;")
    conn.executescript(
        "DROP TABLE IF EXISTS pages_fts; DROP TABLE IF EXISTS pages;"
        "DROP TABLE IF EXISTS feeds;")
    conn.executescript(SCHEMA)
    conn.execute("INSERT OR REPLACE INTO index_meta(key,value) VALUES ('schema',?)",
                 (SCHEMA_VERSION,))
    conn.commit()


def _text_of(url: str, title: str, snippet: str) -> str:
    return ranker_text(f"{unslug(url)} {title} {snippet}")


@dataclass
class IndexStats:
    pages: int
    sites: int
    feeds: int
    age_seconds: float | None
    per_site: dict

    def as_dict(self) -> dict:
        return {
            "pages": self.pages,
            "sites": self.sites,
            "feeds": self.feeds,
            "age_seconds": round(self.age_seconds, 1) if self.age_seconds is not None else None,
            "per_site": self.per_site,
        }


class SiteIndex:
    def __init__(self, path: str = DEFAULT_DB, clock=time.time):
        os.makedirs(os.path.dirname(os.path.abspath(path)) or ".", exist_ok=True)
        self.path = path
        self._clock = clock
        self.conn = sqlite3.connect(path, check_same_thread=False, timeout=30)
        self.conn.execute("PRAGMA journal_mode=WAL")
        self.conn.execute("PRAGMA busy_timeout=30000")
        self.conn.execute("PRAGMA synchronous=NORMAL")
        self.conn.executescript(SCHEMA)
        _migrate(self.conn)
        self.conn.commit()

    def close(self) -> None:
        self.conn.close()

    # -- ingest ---------------------------------------------------------

    def replace_site(self, site_id: str, rows: list[dict], full: bool = False) -> int:
        rows = list({row["url"]: row for row in rows}.values())
        if full and not rows:
            return 0  # a failed/empty ingest must not wipe a good index
        now = self._clock()
        with self.conn:
            if full:
                self.conn.execute(
                    "DELETE FROM pages_fts WHERE rowid IN "
                    "(SELECT rowid FROM pages WHERE site=?)", (site_id,))
                self.conn.execute("DELETE FROM pages WHERE site=?", (site_id,))
                base = self.conn.execute(
                    "SELECT COALESCE(MAX(rowid), 0) FROM pages").fetchone()[0]
                page_values = []
                fts_values = []
                for offset, row in enumerate(rows, 1):
                    rowid = base + offset
                    page_values.append((
                        rowid, row["url"], site_id, row["title"], row["snippet"],
                        row.get("lastmod"), row.get("method", "sitemap"),
                        row["text"], now,
                    ))
                    fts_values.append((rowid, row["url"], site_id, row["text"]))
                self.conn.executemany(
                    """INSERT INTO pages(rowid,url,site,title,snippet,lastmod,method,text,updated)
                       VALUES (?,?,?,?,?,?,?,?,?)""", page_values)
                self.conn.executemany(
                    "INSERT INTO pages_fts(rowid,url,site,text) VALUES (?,?,?,?)",
                    fts_values)
                return len(rows)
            for row in rows:
                existing = self.conn.execute(
                    "SELECT rowid FROM pages WHERE url=?", (row["url"],)).fetchone()
                if existing is not None:
                    rowid = existing[0]
                    self.conn.execute(
                        """UPDATE pages SET site=?,title=?,snippet=?,lastmod=?,method=?,
                           text=?,updated=? WHERE rowid=?""",
                        (site_id, row["title"], row["snippet"], row.get("lastmod"),
                         row.get("method", "sitemap"), row["text"], now, rowid),
                    )
                    self.conn.execute("DELETE FROM pages_fts WHERE rowid=?", (rowid,))
                else:
                    cur = self.conn.execute(
                        """INSERT INTO pages(url,site,title,snippet,lastmod,method,text,updated)
                           VALUES (?,?,?,?,?,?,?,?)""",
                        (row["url"], site_id, row["title"], row["snippet"],
                         row.get("lastmod"), row.get("method", "sitemap"),
                         row["text"], now),
                    )
                    rowid = cur.lastrowid
                self.conn.execute(
                    "INSERT INTO pages_fts(rowid,url,site,text) VALUES (?,?,?,?)",
                    (rowid, row["url"], site_id, row["text"]),
                )
        return len(rows)

    def record_feed(self, url: str, site: str, kind: str, etag: str | None,
                    lastmod: str | None, entries: int) -> None:
        with self.conn:
            self.conn.execute(
                """INSERT OR REPLACE INTO feeds(url,site,kind,etag,lastmod,entries,fetched_at)
                   VALUES (?,?,?,?,?,?,?)""",
                (url, site, kind, etag, lastmod, entries, self._clock()),
            )

    # -- query ----------------------------------------------------------

    def site_pages(self, site_id: str) -> int:
        row = self.conn.execute("SELECT COUNT(*) FROM pages WHERE site=?", (site_id,)).fetchone()
        return int(row[0]) if row else 0

    def query_sites(self, site_ids: list[str], query: str,
                    max_results: int = 40, rows_per_site: int = 250,
                    max_age_days: dict[str, int] | None = None) -> dict[str, list[Candidate]]:
        """Lexical match per site, scored with the same scorer as live adapters."""
        from .text import product_aliases

        terms = []
        for term in (stem_token(t) for t in query_terms(query)):
            if term and term not in terms:
                terms.append(term)
        # Combined forms ("iphone17") are for scoring; as FTS terms they only
        # make the recall expression too strict.
        compounds = {
            a + b for a, b in zip(terms, terms[1:])
            if (a + b) in terms
        }
        retrieval = [t for t in terms if t not in compounds]
        for term in list(retrieval):
            for alias in product_aliases(term):
                if alias not in retrieval:
                    retrieval.append(alias)
        if not retrieval or not site_ids:
            return {}
        pairs = []
        if len(retrieval) == 1:
            pairs = [f'"{retrieval[0]}"']
        for i, first in enumerate(retrieval):
            for second in retrieval[i + 1:]:
                pairs.append(f'("{first}" AND "{second}")')
                if len(pairs) >= 12:
                    break
            if len(pairs) >= 12:
                break
        all_and = " AND ".join(f'"{t}"' for t in retrieval)
        model_terms = [t for t in retrieval if MODEL_TOKEN.match(t)]
        site_expr = " OR ".join(f"site:{s}" for s in site_ids)
        limit = max(rows_per_site, rows_per_site * min(len(site_ids), 8))
        base_sql = ("SELECT p.site, p.url, p.title, p.snippet, p.lastmod, p.method "
                    "FROM pages_fts f JOIN pages p ON p.rowid=f.rowid "
                    "WHERE pages_fts MATCH ? LIMIT ?")
        ranked_sql = ("SELECT p.site, p.url, p.title, p.snippet, p.lastmod, p.method "
                      "FROM pages_fts f JOIN pages p ON p.rowid=f.rowid "
                      "WHERE pages_fts MATCH ? ORDER BY rank LIMIT ?")

        def fetch(inner: str) -> list:
            expr = f"({site_expr}) AND ({inner})"
            total = self.conn.execute(
                "SELECT COUNT(*) FROM pages_fts WHERE pages_fts MATCH ?",
                (expr,)).fetchone()[0]
            sql = base_sql if total <= limit else ranked_sql
            return self.conn.execute(sql, (expr, limit)).fetchall()

        rows = []
        try:
            total_all = self.conn.execute(
                "SELECT COUNT(*) FROM pages_fts WHERE pages_fts MATCH ?",
                (f"({site_expr}) AND ({all_and})",)).fetchone()[0]
            if total_all >= 5:
                rows = fetch(all_and)
            else:
                rows = fetch(" OR ".join(pairs))
                if not rows and model_terms:
                    # A missing model code may still match on the code alone.
                    rows = fetch(" OR ".join(f'"{t}"' for t in model_terms))
        except sqlite3.OperationalError:
            rows = []

        grouped: dict[str, list[dict]] = {}
        now_ts = time.time()
        for row in rows:
            site_id = row[0]
            days = int((max_age_days or {}).get(site_id) or 0)
            if days and row[4]:
                try:
                    age = now_ts - datetime.fromisoformat(row[4]).timestamp()
                    if age > days * 86400:
                        continue
                except (ValueError, TypeError):
                    pass
            grouped.setdefault(site_id, []).append(row)

        out: dict[str, list[Candidate]] = {}
        for site_id, site_rows in grouped.items():
            cands = [
                Candidate(
                    url=row[1], title=row[2] or unslug(row[1]),
                    snippet=row[3] or "", published=iso_date(row[4]),
                    source=site_id, method=row[5] or "sitemap",
                )
                for row in site_rows
            ]
            ranked = score_candidates(query, cands, max_results=max_results)
            if ranked:
                out[site_id] = ranked
        return out

    def query(self, site_ids: list[str], query: str, max_results: int = 40,
              max_age_days: dict[str, int] | None = None) -> list[Candidate]:
        merged: dict[str, Candidate] = {}
        for cands in self.query_sites(site_ids, query, max_results=max_results,
                                      max_age_days=max_age_days).values():
            for cand in cands:
                key = _canonical(cand.url)
                if key not in merged or cand.score > merged[key].score:
                    merged[key] = cand
        return score_candidates(query, list(merged.values()), max_results=max_results)

    # -- reporting ------------------------------------------------------

    def stats(self) -> IndexStats:
        pages = int(self.conn.execute("SELECT COUNT(*) FROM pages").fetchone()[0])
        sites = int(self.conn.execute("SELECT COUNT(DISTINCT site) FROM pages").fetchone()[0])
        feeds = int(self.conn.execute("SELECT COUNT(*) FROM feeds").fetchone()[0])
        newest = self.conn.execute("SELECT MAX(updated) FROM pages").fetchone()[0]
        per_site = dict(self.conn.execute(
            "SELECT site, COUNT(*) FROM pages GROUP BY site ORDER BY COUNT(*) DESC").fetchall())
        age = (self._clock() - newest) if newest else None
        return IndexStats(pages, sites, feeds, age, per_site)


def _canonical(url: str) -> str:
    parts = urlparse(url)
    path = parts.path or "/"
    if path != "/" and path.endswith("/"):
        path = path[:-1]
    return f"{parts.scheme.lower()}://{parts.netloc.lower()}{path}?{parts.query}"


def _header(headers: dict | None, name: str) -> str | None:
    for key, value in (headers or {}).items():
        if key.lower() == name:
            return value
    return None


def _select_children(entries: list[dict], method: dict, limit: int) -> list[dict]:
    include = [re.compile(p) for p in method.get("include_children", [])]
    exclude = [re.compile(p) for p in method.get("exclude_children", [])]
    selected = []
    for item in entries:
        loc = item["loc"]
        if include and not any(p.search(loc) for p in include):
            continue
        if exclude and any(p.search(loc) for p in exclude):
            continue
        selected.append(item)

    def key(item: dict) -> str:
        loc = item.get("loc", "")
        found = re.search(r"(19\d{2}|20\d{2})[-_](\d{1,2})(?=[^0-9]|$)", loc)
        if found:
            return f"{found.group(1)}-{int(found.group(2)):02d}"
        return item.get("lastmod") or ""

    selected.sort(key=key, reverse=True)
    return selected[:limit] if limit else selected


async def ingest_site(site: dict, fetcher, index: SiteIndex, full: bool = True,
                      recent: int = 12) -> dict:
    """Ingest one site's sitemaps/RSS into the index. Never fetches pages."""
    t0 = time.monotonic()
    rows: list[dict] = []
    feeds = 0
    errors: list[str] = []
    robots = 0
    for method in site["methods"]:
        kind = method.get("type")
        if kind == "sitemap":
            urls = list(method.get("urls") or ([method["url"]] if method.get("url") else []))
            cap = site.get("index_max_children")
            if full and cap:
                limit = int(cap)
            else:
                limit = 0 if full else recent
            for url in urls:
                try:
                    res = await fetcher.get(url, conditional=True)
                except Exception as exc:
                    errors.append(f"{url}: {exc!r}"[:160])
                    continue
                if res.robots_skipped:
                    robots += 1
                    continue
                if not res.ok:
                    errors.append(f"{url}: HTTP {res.status}")
                    continue
                index.record_feed(url, site["id"], "sitemapindex",
                                  _header(res.headers, "etag"),
                                  _header(res.headers, "last-modified"), 0)
                if res.not_modified:
                    continue
                root_kind, entries = parse_xml(res.text)
                children = []
                if root_kind == "sitemapindex":
                    children = _select_children(entries, method, limit)
                else:
                    children = [{"loc": url}]
                for child in children:
                    child_url = child["loc"]
                    if child_url != url:
                        try:
                            res2 = await fetcher.get(child_url, conditional=True)
                        except Exception as exc:
                            errors.append(f"{child_url}: {exc!r}"[:160])
                            continue
                        if res2.robots_skipped:
                            robots += 1
                            continue
                        if not res2.ok:
                            errors.append(f"{child_url}: HTTP {res2.status}")
                            continue
                        if res2.not_modified:
                            continue
                        child_kind, child_entries = parse_xml(res2.text)
                        headers = res2.headers or {}
                    else:
                        child_kind, child_entries = root_kind, entries
                        headers = res.headers or {}
                    index.record_feed(child_url, site["id"], child_kind,
                                      _header(headers, "etag"), _header(headers, "last-modified"),
                                      len(child_entries))
                    feeds += 1
                    rows.extend(_rows_from_entries(site, child_entries, child_kind))
        elif kind == "rss":
            url = method["url"]
            try:
                res = await fetcher.get(url, conditional=True)
            except Exception as exc:
                errors.append(f"{url}: {exc!r}"[:160])
                continue
            if res.robots_skipped:
                robots += 1
                continue
            if not res.ok:
                errors.append(f"{url}: HTTP {res.status}")
                continue
            index.record_feed(url, site["id"], "rss", _header(res.headers, "etag"),
                              _header(res.headers, "last-modified"), 0)
            if not res.not_modified:
                feed_kind, entries = parse_xml(res.text)
                feeds += 1
                rows.extend(_rows_from_entries(site, entries, feed_kind))
    # A full build replaces the site; a refresh only upserts what it saw.
    written = index.replace_site(site["id"], rows, full=full)
    return {
        "site": site["id"], "pages": written, "feeds": feeds,
        "robots_skips": robots, "errors": errors[:8],
        "seconds": round(time.monotonic() - t0, 1),
    }


def _rows_from_entries(site: dict, entries: list[dict], kind: str) -> list[dict]:
    domain = site["domain"].lower()
    rows = []
    for item in entries:
        url = item.get("loc") or item.get("link") or ""
        if not url or ASSET_EXT.search(url):
            continue
        if domain not in (urlparse(url).netloc.lower() or ""):
            continue
        if kind == "rss":
            title = " ".join((item.get("title") or "").split())[:300] or unslug(url)
            snippet = " ".join((item.get("description") or "").split())[:500]
            lastmod = item.get("pubDate")
        else:
            title = (item.get("title") or unslug(url))[:300]
            snippet = f"{site.get('name', '')} {unslug(url)}"[:500]
            lastmod = item.get("lastmod")
        rows.append({
            "url": url, "title": title, "snippet": snippet,
            "lastmod": iso_date(lastmod),
            "method": "rss" if kind == "rss" else "sitemap",
            "text": _text_of(url, title, snippet),
        })
    return rows


async def build(sites: list[str] | None = None, db: str = DEFAULT_DB, full: bool = True,
                recent: int = 12, concurrency: int = 4) -> dict:
    from fetchlib import Fetcher
    from .log import get_logger

    log = get_logger("sites.index")
    registry = load_registry()
    selected = [s for s in registry["sites"] if not sites or s["id"] in sites]
    index = SiteIndex(db)
    fetcher = Fetcher(concurrency=concurrency)
    try:
        async def one(site):
            row = None
            for attempt in range(2):
                try:
                    row = await ingest_site(site, fetcher, index, full=full, recent=recent)
                    break
                except Exception as exc:  # one bad site must not abort the build
                    if isinstance(exc, sqlite3.OperationalError) and attempt == 0:
                        await asyncio.sleep(2)
                        continue
                    row = {"site": site["id"], "pages": 0, "feeds": 0,
                           "robots_skips": 0, "errors": [repr(exc)[:160]], "seconds": 0.0}
                    break
            log.info("site indexed", extra={"site": row["site"], "pages": row["pages"],
                                            "status": "ok" if not row["errors"] else "error",
                                            "ms": round(row["seconds"] * 1000)})
            return row

        outcomes = await asyncio.gather(*[one(s) for s in selected])
        return {"mode": "full" if full else "refresh", "sites": outcomes,
                "index": index.stats().as_dict()}
    finally:
        await fetcher.close()
        index.close()


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="Build/refresh the local sitemap index")
    ap.add_argument("command", choices=["build", "refresh", "stats"])
    ap.add_argument("--site", action="append")
    ap.add_argument("--db", default=DEFAULT_DB)
    ap.add_argument("--recent", type=int, default=12,
                    help="child sitemaps kept on refresh (all are kept on build)")
    ap.add_argument("--concurrency", type=int, default=4)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)

    if args.command == "stats":
        index = SiteIndex(args.db)
        try:
            stats = index.stats().as_dict()
        finally:
            index.close()
        print(json.dumps(stats, ensure_ascii=False, indent=1))
        return 0

    result = asyncio.run(build(args.site, args.db, full=args.command == "build",
                               recent=args.recent, concurrency=args.concurrency))
    if args.json:
        print(json.dumps(result, ensure_ascii=False, indent=1))
    else:
        for row in result["sites"]:
            print(f"{row['site']:<16} pages={row['pages']:<6} feeds={row['feeds']:<4} "
                  f"robots={row['robots_skips']:<2} {row['seconds']:>6.1f}s "
                  f"{'; '.join(row['errors'])[:70]}")
        print(json.dumps(result["index"], ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
