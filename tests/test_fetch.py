import asyncio

import pytest

from fetchlib.http import Fetcher, RateLimiter


class FakeResponse:
    def __init__(self, status=200, body=b"", headers=None, url=""):
        self.status_code = status
        self.content = body
        self.headers = headers or {}
        self.url = url


class FakeTransport:
    def __init__(self, routes):
        self.routes = routes
        self.calls = []

    async def get(self, url, headers, timeout):
        self.calls.append(url)
        item = self.routes.get(url)
        if callable(item):
            return item(url)
        if item is None:
            return FakeResponse(404, b"not found", {"content-type": "text/plain"}, url)
        status, body, headers = item
        return FakeResponse(status, body.encode() if isinstance(body, str) else body, headers, url)

    async def close(self):
        pass


class FakeClock:
    def __init__(self):
        self.t = 0.0
        self.sleeps = []

    def now(self):
        return self.t

    async def sleep(self, delay):
        self.sleeps.append(delay)
        self.t += delay


def make_fetcher(tmp_path, routes, **kwargs):
    clock = FakeClock()
    kwargs.setdefault("min_interval", 0)
    return Fetcher(
        cache_path=str(tmp_path / "cache.sqlite"),
        transport=FakeTransport(routes),
        clock=clock.now,
        sleep=clock.sleep,
        **kwargs,
    ), clock


def test_disallowed_path_is_never_fetched(tmp_path):
    robots = "User-agent: *\nDisallow: /private\nAllow: /\n"
    routes = {
        "https://example.com/robots.txt": (200, robots, {"content-type": "text/plain"}),
        "https://example.com/public": (200, "<html><body>hello</body></html>", {"content-type": "text/html"}),
    }
    fetcher, _ = make_fetcher(tmp_path, routes)

    async def run():
        denied = await fetcher.get("https://example.com/private/page")
        allowed = await fetcher.get("https://example.com/public")
        return denied, allowed, fetcher.transport.calls

    denied, allowed, calls = asyncio.run(run())
    assert denied.robots_skipped is True
    assert denied.status == "robots-denied"
    assert "https://example.com/private/page" not in calls
    assert allowed.status == 200
    assert "https://example.com/public" in calls


def test_wildcard_disallow_is_honoured(tmp_path):
    robots = "User-agent: *\nDisallow: /*?q=*\n"
    routes = {
        "https://example.com/robots.txt": (200, robots, {"content-type": "text/plain"}),
    }
    fetcher, _ = make_fetcher(tmp_path, routes)

    async def run():
        return await fetcher.get("https://example.com/search?q=phone")

    result = asyncio.run(run())
    assert result.status == "robots-denied"
    assert fetcher.transport.calls == ["https://example.com/robots.txt"]


def test_robots_403_denies_everything(tmp_path):
    routes = {"https://example.com/robots.txt": (403, "no", {"content-type": "text/plain"})}
    fetcher, _ = make_fetcher(tmp_path, routes)

    async def run():
        return await fetcher.get("https://example.com/")

    assert asyncio.run(run()).status == "robots-denied"


def test_html_robots_error_is_treated_as_unavailable(tmp_path):
    routes = {
        "https://guvensizurun.example/robots.txt": (200, "<html><body>URL reddedildi</body></html>", {"content-type": "text/html"}),
        "https://guvensizurun.example/Genel/Rss": (200, "<rss></rss>", {"content-type": "text/xml"}),
    }
    fetcher, _ = make_fetcher(tmp_path, routes)

    async def run():
        return await fetcher.get("https://guvensizurun.example/Genel/Rss")

    result = asyncio.run(run())
    assert result.status == 200
    assert result.robots_allowed is True


def test_rate_limiter_spaces_requests_and_honours_crawl_delay():
    clock = FakeClock()
    limiter = RateLimiter(1.0, clock=clock.now, sleep=clock.sleep)

    async def run():
        first = await limiter.wait("a.example")
        second = await limiter.wait("a.example")
        third = await limiter.wait("a.example", crawl_delay=3.0)
        other = await limiter.wait("b.example")
        return first, second, third, other

    first, second, third, other = asyncio.run(run())
    assert first == 0
    assert second == pytest.approx(1.0)
    assert third == pytest.approx(3.0)
    assert other == 0


def test_retry_after_is_respected(tmp_path):
    calls = {"n": 0}

    def route(url):
        calls["n"] += 1
        if calls["n"] == 1:
            return FakeResponse(429, b"slow down", {"retry-after": "2"}, url)
        return FakeResponse(200, b"<html>ok</html>", {"content-type": "text/html"}, url)

    routes = {
        "https://example.com/robots.txt": (200, "User-agent: *\nAllow: /\n", {"content-type": "text/plain"}),
        "https://example.com/page": route,
    }
    fetcher, clock = make_fetcher(tmp_path, routes, min_interval=0)

    async def run():
        return await fetcher.get("https://example.com/page")

    result = asyncio.run(run())
    assert result.status == 200
    assert result.text == "<html>ok</html>"
    assert 2.0 in clock.sleeps
    assert calls["n"] == 2


def test_conditional_get_serves_cached_body_on_304(tmp_path):
    calls = {"n": 0}

    def page(url):
        calls["n"] += 1
        if calls["n"] == 1:
            return FakeResponse(200, b"<html>v1</html>",
                                {"content-type": "text/html", "etag": '"v1"'}, url)
        return FakeResponse(304, b"", {"etag": '"v1"'}, url)

    routes = {
        "https://example.com/robots.txt": (200, "User-agent: *\nAllow: /\n", {"content-type": "text/plain"}),
        "https://example.com/page": page,
    }
    now = [1000.0]
    fetcher, _ = make_fetcher(tmp_path, routes, cache_ttl=60, now_fn=lambda: now[0])

    async def run():
        first = await fetcher.get("https://example.com/page")
        now[0] += 61
        second = await fetcher.get("https://example.com/page", conditional=True)
        third = await fetcher.get("https://example.com/page", conditional=True)
        return first, second, third

    first, second, third = asyncio.run(run())
    assert first.text == "<html>v1</html>"
    assert second.not_modified is True and second.ok
    assert second.text == "<html>v1</html>"
    assert third.from_cache is True  # the 304 refreshed the TTL
    assert calls["n"] == 2


def test_page_cache_ttl(tmp_path):
    routes = {
        "https://example.com/robots.txt": (200, "User-agent: *\nAllow: /\n", {"content-type": "text/plain"}),
        "https://example.com/page": (200, "<html>v1</html>", {"content-type": "text/html"}),
    }
    now = [1000.0]
    fetcher, _ = make_fetcher(tmp_path, routes, cache_ttl=60, now_fn=lambda: now[0])

    async def run():
        first = await fetcher.get("https://example.com/page")
        second = await fetcher.get("https://example.com/page")
        now[0] += 61
        third = await fetcher.get("https://example.com/page")
        return first, second, third

    first, second, third = asyncio.run(run())
    assert first.from_cache is False
    assert second.from_cache is True
    assert third.from_cache is False
    assert fetcher.transport.calls.count("https://example.com/page") == 2
