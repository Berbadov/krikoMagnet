"""Polite HTTP fetching for Kriko Magnet.

Shared by Stage 2, Stage 3 and the site adapters. It identifies honestly,
checks robots.txt, spaces requests per host (honouring Crawl-delay), caps
global concurrency, backs off on 429/503, and caches responses with a TTL.

Chrome TLS impersonation is opt-in only (``impersonate="chrome"``) and off by
default.
"""
from __future__ import annotations

import asyncio
import hashlib
import json
import os
import random
import re
import sqlite3
import threading
import time
from dataclasses import dataclass, field
from email.utils import parsedate_to_datetime
from typing import Any, Callable, Optional
from urllib.parse import urlparse

from curl_cffi import requests as cr

try:  # protego handles robots.txt wildcards and Crawl-delay correctly
    from protego import Protego
except Exception:  # pragma: no cover - fallback for minimal installs
    Protego = None

USER_AGENT = "KrikoMagnet/0.2 (+https://kriko.cc/bot)"
DEFAULT_DATA_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "data")
DEFAULT_CACHE = os.path.join(DEFAULT_DATA_DIR, "fetch_cache.sqlite")

ROBOTS_TTL = 24 * 3600
PAGE_TTL = 24 * 3600
MAX_CACHE_BYTES = 8_000_000
MAX_RETRIES = 3
MAX_BACKOFF = 30.0


def host_of(url: str) -> str:
    return (urlparse(url).netloc or "").lower()


def origin_of(url: str) -> str:
    p = urlparse(url)
    return f"{p.scheme}://{p.netloc.lower()}"


def sha16(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()[:16]


def decode_body(body: bytes, headers: dict) -> str:
    if body[:2] == b"\x1f\x8b":
        try:
            import gzip
            body = gzip.decompress(body)
        except Exception:
            pass
    ctype = headers.get("content-type") or headers.get("Content-Type") or ""
    m = re.search(r"charset=([\w-]+)", ctype, re.I)
    encodings = [m.group(1)] if m else []
    encodings += ["utf-8", "latin-1"]
    for enc in encodings:
        try:
            return body.decode(enc)
        except (LookupError, UnicodeDecodeError):
            continue
    return body.decode("utf-8", errors="replace")


def retry_after_seconds(value: Optional[str], now: Optional[float] = None) -> Optional[float]:
    if not value:
        return None
    value = value.strip()
    try:
        return max(0.0, float(value))
    except ValueError:
        pass
    try:
        dt = parsedate_to_datetime(value)
        return max(0.0, dt.timestamp() - (now if now is not None else time.time()))
    except Exception:
        return None


class RateLimiter:
    """One request slot per host every ``min_interval`` seconds."""

    def __init__(self, min_interval: float = 1.0, clock: Callable[[], float] = time.monotonic,
                 sleep: Callable[[float], Any] = asyncio.sleep):
        self.min_interval = min_interval
        self._clock = clock
        self._sleep = sleep
        self._last: dict[str, float] = {}
        self._locks: dict[str, asyncio.Lock] = {}

    def lock_for(self, host: str) -> asyncio.Lock:
        lock = self._locks.get(host)
        if lock is None:
            lock = asyncio.Lock()
            self._locks[host] = lock
        return lock

    async def wait(self, host: str, crawl_delay: Optional[float] = None) -> float:
        interval = max(self.min_interval, crawl_delay or 0.0)
        slept = 0.0
        async with self.lock_for(host):
            last = self._last.get(host)
            if last is not None:
                delay = last + interval - self._clock()
                if delay > 0:
                    slept = delay
                    await self._sleep(delay)
            self._last[host] = self._clock()
        return slept


class ResponseCache:
    """SQLite response cache with a TTL."""

    def __init__(self, path: str = DEFAULT_CACHE):
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        self.path = path
        self._lock = threading.Lock()
        self.conn = sqlite3.connect(path, check_same_thread=False, timeout=30)
        self.conn.execute("PRAGMA journal_mode=WAL")
        self.conn.execute("PRAGMA busy_timeout=30000")
        self.conn.execute(
            """CREATE TABLE IF NOT EXISTS http_cache(
                   url TEXT PRIMARY KEY, status INTEGER, headers TEXT,
                   body BLOB, fetched_at REAL)"""
        )
        self.conn.commit()

    def get(self, url: str, ttl: float, now: Optional[float] = None) -> Optional[dict]:
        now = time.time() if now is None else now
        with self._lock:
            row = self.conn.execute(
                "SELECT status, headers, body, fetched_at FROM http_cache WHERE url=?", (url,)
            ).fetchone()
        if row is None:
            return None
        status, headers, body, fetched_at = row
        if ttl <= 0 or now - fetched_at > ttl:
            return None
        try:
            headers = json.loads(headers)
        except Exception:
            headers = {}
        return {"status": status, "headers": headers, "body": body, "fetched_at": fetched_at}

    def get_raw(self, url: str) -> Optional[dict]:
        """A cached response regardless of TTL (for conditional requests)."""
        with self._lock:
            row = self.conn.execute(
                "SELECT status, headers, body, fetched_at FROM http_cache WHERE url=?", (url,)
            ).fetchone()
        if row is None:
            return None
        try:
            headers = json.loads(row[1])
        except Exception:
            headers = {}
        return {"status": row[0], "headers": headers, "body": row[2], "fetched_at": row[3]}

    def set(self, url: str, status: int, headers: dict, body: bytes, now: Optional[float] = None) -> None:
        if len(body) > MAX_CACHE_BYTES:
            return
        now = time.time() if now is None else now
        with self._lock:
            self.conn.execute(
                "INSERT OR REPLACE INTO http_cache(url,status,headers,body,fetched_at) VALUES (?,?,?,?,?)",
                (url, status, json.dumps(headers), body, now),
            )
            self.conn.commit()

    def touch(self, url: str, now: Optional[float] = None) -> None:
        """Mark a cached response fresh after a 304 Not Modified."""
        now = time.time() if now is None else now
        with self._lock:
            self.conn.execute("UPDATE http_cache SET fetched_at=? WHERE url=?", (now, url))
            self.conn.commit()

    def clear(self) -> None:
        with self._lock:
            self.conn.execute("DELETE FROM http_cache")
            self.conn.commit()

    def close(self) -> None:
        self.conn.close()


class RobotsCache:
    """Cached robots.txt bodies plus parsed policies."""

    def __init__(self, path: str = DEFAULT_CACHE):
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        self.path = path
        self._lock = threading.Lock()
        self.conn = sqlite3.connect(path, check_same_thread=False, timeout=30)
        self.conn.execute("PRAGMA journal_mode=WAL")
        self.conn.execute("PRAGMA busy_timeout=30000")
        self.conn.execute(
            """CREATE TABLE IF NOT EXISTS robots(
                   origin TEXT PRIMARY KEY, status INTEGER, body TEXT, fetched_at REAL)"""
        )
        self.conn.commit()
        self._parsed: dict[str, Any] = {}

    def get(self, origin: str, ttl: float = ROBOTS_TTL, now: Optional[float] = None) -> Optional[dict]:
        now = time.time() if now is None else now
        with self._lock:
            row = self.conn.execute(
                "SELECT status, body, fetched_at FROM robots WHERE origin=?", (origin,)
            ).fetchone()
        if row is None or now - row[2] > ttl:
            return None
        return {"status": row[0], "body": row[1], "fetched_at": row[2]}

    def set(self, origin: str, status: int, body: str, now: Optional[float] = None) -> None:
        now = time.time() if now is None else now
        with self._lock:
            self.conn.execute(
                "INSERT OR REPLACE INTO robots(origin,status,body,fetched_at) VALUES (?,?,?,?)",
                (origin, status, body, now),
            )
            self.conn.commit()
        for key in [k for k in self._parsed if k[0] == origin]:
            self._parsed.pop(key, None)

    def policy(self, origin: str, user_agent: str):
        key = (origin, user_agent)
        if key in self._parsed:
            return self._parsed[key]
        row = self.get(origin)
        policy = None
        if row is not None:
            policy = RobotsPolicy(row["body"], row["status"], user_agent)
        self._parsed[key] = policy
        return policy

    def close(self) -> None:
        self.conn.close()


class RobotsPolicy:
    """Allow/deny decisions with a conservative fallback."""

    def __init__(self, body: str, status: int, user_agent: str):
        self.status = status
        self.user_agent = user_agent
        self.unknown = False
        self.deny_all = status in (401, 403, 429) or 500 <= status <= 599
        self.body = body or ""
        self.parser = None
        if self.body and "<html" not in self.body[:300].lower():
            if Protego is not None:
                try:
                    self.parser = Protego.parse(self.body)
                except Exception:
                    self.parser = None
            if self.parser is None:
                from urllib.robotparser import RobotFileParser
                rp = RobotFileParser()
                rp.parse(self.body.splitlines())
                self.parser = rp
        else:
            # A non-robots response (HTML error page, empty body) means the
            # file is unavailable; we do not treat that as a denial.
            self.unknown = True

    def allowed(self, url: str) -> bool:
        if self.deny_all:
            return False
        if self.parser is None:
            return True
        try:
            if Protego is not None and isinstance(self.parser, Protego):
                return bool(self.parser.can_fetch(url, self.user_agent))
            return bool(self.parser.can_fetch(self.user_agent, url))
        except Exception:
            return True

    def crawl_delay(self) -> Optional[float]:
        if self.parser is None:
            return None
        try:
            if Protego is not None and isinstance(self.parser, Protego):
                value = self.parser.crawl_delay(self.user_agent)
            else:
                value = self.parser.crawl_delay(self.user_agent)
            return float(value) if value else None
        except Exception:
            return None


@dataclass
class FetchResult:
    url: str
    final_url: str = ""
    status: int | str = 0
    body: bytes = b""
    text: str = ""
    headers: dict = field(default_factory=dict)
    error: Optional[str] = None
    from_cache: bool = False
    not_modified: bool = False
    robots_allowed: Optional[bool] = None
    robots_skipped: bool = False
    elapsed_ms: float = 0.0

    @property
    def ok(self) -> bool:
        return self.status == 200 and not self.error

    @property
    def size(self) -> int:
        return len(self.body)


class CurlTransport:
    def __init__(self, impersonate: Optional[str] = None):
        self.impersonate = impersonate
        self.session = cr.AsyncSession(impersonate=impersonate) if impersonate else cr.AsyncSession()

    async def get(self, url: str, headers: dict, timeout: float):
        return await self.session.get(url, headers=headers, timeout=timeout, allow_redirects=True)

    async def close(self):
        await self.session.close()


class Fetcher:
    def __init__(
        self,
        cache_path: str = DEFAULT_CACHE,
        cache_ttl: float = PAGE_TTL,
        robots_ttl: float = ROBOTS_TTL,
        min_interval: float = 1.0,
        concurrency: int = 8,
        user_agent: str = USER_AGENT,
        impersonate: Optional[str] = None,
        transport: Any = None,
        clock: Callable[[], float] = time.monotonic,
        sleep: Callable[[float], Any] = asyncio.sleep,
        now_fn: Callable[[], float] = time.time,
    ):
        self.cache = ResponseCache(cache_path)
        self.robots = RobotsCache(cache_path)
        self.cache_ttl = cache_ttl
        self.robots_ttl = robots_ttl
        self.user_agent = user_agent
        self.limiter = RateLimiter(min_interval, clock=clock, sleep=sleep)
        self.sem = asyncio.Semaphore(concurrency)
        self.transport = transport or CurlTransport(impersonate)
        self._sleep = sleep
        self._now = now_fn
        self._robots_locks: dict[str, asyncio.Lock] = {}
        self.stats = {"requests": 0, "cache_hits": 0, "robots_skips": 0, "retries": 0, "bytes": 0}

    async def close(self):
        close = getattr(self.transport, "close", None)
        if close is not None:
            await close()
        self.cache.close()
        self.robots.close()

    async def robots_policy(self, url: str) -> Optional[RobotsPolicy]:
        origin = origin_of(url)
        policy = self.robots.policy(origin, self.user_agent)
        if policy is not None:
            return policy
        lock = self._robots_locks.get(origin)
        if lock is None:
            lock = asyncio.Lock()
            self._robots_locks[origin] = lock
        async with lock:
            policy = self.robots.policy(origin, self.user_agent)
            if policy is not None:
                return policy
            res = await self._request(origin + "/robots.txt", headers={}, timeout=20.0)
            body = res.text if res.status not in ("ERR",) and res.status != 0 else ""
            status = res.status if isinstance(res.status, int) else 0
            self.robots.set(origin, status, body, now=self._now())
            policy = RobotsPolicy(body, status, self.user_agent)
            self.robots._parsed[(origin, self.user_agent)] = policy
            return policy

    async def allowed(self, url: str) -> Optional[bool]:
        policy = await self.robots_policy(url)
        if policy is None:
            return None
        return policy.allowed(url)

    async def get(
        self,
        url: str,
        check_robots: bool = True,
        use_cache: bool = True,
        timeout: float = 25.0,
        headers: Optional[dict] = None,
        conditional: bool = False,
    ) -> FetchResult:
        t0 = time.monotonic()
        result = FetchResult(url=url, final_url=url)
        crawl_delay = None
        if check_robots:
            policy = await self.robots_policy(url)
            if policy is not None:
                result.robots_allowed = policy.allowed(url)
                crawl_delay = policy.crawl_delay()
                if not result.robots_allowed:
                    self.stats["robots_skips"] += 1
                    result.status = "robots-denied"
                    result.robots_skipped = True
                    result.elapsed_ms = (time.monotonic() - t0) * 1000
                    return result

        stale = None
        if use_cache:
            cached = self.cache.get(url, self.cache_ttl, now=self._now())
            if cached is not None:
                self.stats["cache_hits"] += 1
                headers_out = cached["headers"]
                result.status = cached["status"]
                result.body = cached["body"]
                result.headers = headers_out
                result.text = decode_body(cached["body"], headers_out)
                result.from_cache = True
                result.elapsed_ms = (time.monotonic() - t0) * 1000
                return result
            if conditional:
                stale = self.cache.get_raw(url)

        req_headers = {"User-Agent": self.user_agent}
        if headers:
            req_headers.update(headers)
        if stale is not None:
            old = stale["headers"] or {}
            etag = old.get("etag") or old.get("ETag")
            last_mod = old.get("last-modified") or old.get("Last-Modified")
            if etag:
                req_headers["If-None-Match"] = etag
            if last_mod:
                req_headers["If-Modified-Since"] = last_mod
            if not etag and not last_mod:
                stale = None

        for attempt in range(MAX_RETRIES + 1):
            await self.limiter.wait(host_of(url), crawl_delay)
            try:
                async with self.sem:
                    resp = await self.transport.get(url, req_headers, timeout)
                status = int(resp.status_code)
                body = resp.content
                resp_headers = dict(resp.headers or {})
                final_url = str(getattr(resp, "url", url))
            except Exception as exc:
                result.error = repr(exc)[:200]
                result.status = "ERR"
                if attempt < MAX_RETRIES:
                    self.stats["retries"] += 1
                    await self._sleep((1.0 + attempt) * random.uniform(0.75, 1.0))
                    continue
                result.elapsed_ms = (time.monotonic() - t0) * 1000
                return result

            if status == 304 and stale is not None:
                self.stats["requests"] += 1
                self.cache.touch(url, now=self._now())
                headers_out = stale["headers"]
                result.status = 200
                result.body = stale["body"]
                result.headers = headers_out
                result.text = decode_body(stale["body"], headers_out)
                result.from_cache = True
                result.not_modified = True
                result.final_url = final_url
                result.elapsed_ms = (time.monotonic() - t0) * 1000
                return result

            self.stats["requests"] += 1
            self.stats["bytes"] += len(body)
            result.status = status
            result.body = body
            result.headers = resp_headers
            result.text = decode_body(body, resp_headers)
            result.final_url = final_url

            if status in (429, 500, 502, 503, 504) and attempt < MAX_RETRIES:
                retry_after = retry_after_seconds(resp_headers.get("retry-after"), self._now())
                if retry_after is not None and status in (429, 503):
                    wait = min(MAX_BACKOFF, max(1.0, retry_after))
                else:
                    wait = min(MAX_BACKOFF, 2.0 * (attempt + 1) * random.uniform(0.75, 1.0))
                self.stats["retries"] += 1
                await self._sleep(wait)
                continue
            break

        if result.status == 200 and use_cache:
            self.cache.set(url, result.status, result.headers, result.body, now=self._now())
        result.elapsed_ms = (time.monotonic() - t0) * 1000
        return result

    async def _request(self, url: str, headers: dict, timeout: float) -> FetchResult:
        """One raw request, bypassing robots and the response cache."""
        result = FetchResult(url=url, final_url=url)
        try:
            await self.limiter.wait(host_of(url))
            async with self.sem:
                resp = await self.transport.get(url, headers or {"User-Agent": self.user_agent}, timeout)
            result.status = int(resp.status_code)
            result.body = resp.content
            result.headers = dict(resp.headers or {})
            result.text = decode_body(result.body, result.headers)
            result.final_url = str(getattr(resp, "url", url))
            self.stats["requests"] += 1
            self.stats["bytes"] += len(result.body)
        except Exception as exc:
            result.error = repr(exc)[:200]
            result.status = "ERR"
        return result
