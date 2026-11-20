"""MCP server tests: real stdio protocol, no network.

The server subprocess loads tests/mcp_fake_engine.py through MAGNET_MCP_ENGINE,
so every tool call exercises the production engine and protocol paths against
stub inputs.
"""
from __future__ import annotations

import asyncio
import json
import os
import sys

import pytest
from mcp import ClientSession
from mcp.client.stdio import StdioServerParameters, stdio_client
from mcp.shared.exceptions import MCPError

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TESTS = os.path.dirname(os.path.abspath(__file__))
TOOLS = {"magnet_search", "magnet_fetch", "magnet_health"}


def _server_params():
    env = dict(os.environ)
    env["MAGNET_MCP_ENGINE"] = "mcp_fake_engine:make_engine"
    env["PYTHONPATH"] = os.pathsep.join([ROOT, TESTS])
    return StdioServerParameters(command=sys.executable, args=["-m", "mcp_server"],
                                 env=env, cwd=ROOT)


async def _with_session(fn):
    async with stdio_client(_server_params()) as (read, write):
        async with ClientSession(read, write) as session:
            init = await session.initialize()
            return await fn(session, init)


def _run(fn):
    return asyncio.run(_with_session(fn))


async def _call(session, name, arguments):
    result = await session.call_tool(name, arguments)
    text = " ".join(block.text for block in result.content if getattr(block, "text", None))
    return result, text


async def _call_json(session, name, arguments):
    result, text = await _call(session, name, arguments)
    assert result.is_error is False, text
    return json.loads(text)


def test_initialize_and_list_tools():
    async def run(session, init):
        return init, await session.list_tools()

    init, listed = _run(run)
    assert init.server_info.name == "magnet"
    assert "robots.txt" in (init.instructions or "")
    assert {tool.name for tool in listed.tools} == TOOLS

    search = next(tool for tool in listed.tools if tool.name == "magnet_search")
    assert search.input_schema["required"] == ["query"]
    assert set(search.input_schema["properties"]) == {"query", "k", "lang", "fetch"}
    assert search.input_schema["properties"]["fetch"]["default"] is True

    health = next(tool for tool in listed.tools if tool.name == "magnet_health")
    assert health.input_schema["properties"]["probe"]["default"] is False


def test_search_shapes_results_and_skips():
    async def run(session, init):
        return await _call_json(session, "magnet_search",
                               {"query": "lg c1 burn in", "k": 5})

    payload = _run(run)
    assert payload["query"] == "lg c1 burn in"
    assert payload["pages_fetched"] is True
    assert payload["plan"]["sites"] == ["rtings"]

    results = payload["results"]
    assert [row["site"] for row in results] == ["rtings.com", "donanimhaber.com"]
    top = results[0]
    assert set(top) == {"rank", "title", "url", "site", "snippet", "score",
                        "cached", "fetched", "status", "source"}
    assert top["cached"] is True and top["fetched"] is True
    assert len(top["snippet"]) <= 280
    assert top["score"] == 1.5

    reasons = [skip["reason"] for skip in payload["skipped"]]
    assert any("robots.txt" in reason for reason in reasons)
    assert "HTTP 403" in reasons

    polite = payload["polite"]
    assert polite["min_host_interval_s"] == 1.0
    assert polite["user_agent"].startswith("KrikoMagnet/")
    assert polite["http_requests"] == 0
    assert polite["robots_skips"] == 0


def test_search_without_page_fetch():
    async def run(session, init):
        return await _call_json(session, "magnet_search",
                               {"query": "lg c1 burn in", "k": 2, "fetch": False})

    payload = _run(run)
    assert payload["pages_fetched"] is False
    assert len(payload["results"]) == 2
    assert all(row["status"] == "not-fetched" for row in payload["results"])
    assert all(row["fetched"] is False and row["cached"] is False
               for row in payload["results"])
    assert payload["results"][0]["snippet"] == "burn-in test"
    assert payload["polite"]["http_requests"] == 0


def test_lang_prefers_registered_language():
    async def run(session, init):
        tr = await _call_json(session, "magnet_search",
                              {"query": "lg c1 burn in", "k": 5, "lang": "tr"})
        en = await _call_json(session, "magnet_search",
                              {"query": "lg c1 burn in", "k": 5, "lang": "en"})
        return tr, en

    tr, en = _run(run)
    assert [row["site"] for row in tr["results"]] == ["donanimhaber.com"]
    assert [row["site"] for row in en["results"]] == ["rtings.com"]


def test_fetch_extracts_text_and_refuses_robots():
    async def run(session, init):
        ok = await _call_json(session, "magnet_fetch", {"url": "https://example.com/page"})
        blocked = await _call_json(session, "magnet_fetch",
                                   {"url": "https://example.com/blocked"})
        missing = await _call_json(session, "magnet_fetch",
                                   {"url": "https://example.com/missing"})
        return ok, blocked, missing

    ok, blocked, missing = _run(run)
    assert ok["ok"] is True
    assert "burn-in" in ok["text"]
    assert ok["robots_allowed"] is True and ok["cached"] is False

    assert blocked["ok"] is False
    assert blocked["robots_allowed"] is False
    assert "robots.txt disallows" in blocked["error"]

    assert missing["ok"] is False
    assert missing["error"] == "HTTP 404"


def test_health_reports_local_state():
    async def run(session, init):
        return await _call_json(session, "magnet_health", {})

    report = _run(run)
    assert report["ok"] is True
    assert report["sites"] == 3
    assert report["by_language"] == {"en": 2, "tr": 1}
    assert report["index"]["missing"] is True
    assert isinstance(report["ranker"]["binary"], bool)
    assert report["circuit_open"] == []
    assert "probe" not in report


def test_health_probe_summarises_sites(monkeypatch, tmp_path):
    import sites.healthcheck as healthcheck

    from mcp_server.engine import MagnetEngine

    async def fake_run(site_ids=None, test_extract=True, breaker=None):
        return [{"site": "rtings", "status": "ok", "links": 3,
                 "circuit_open": False, "secs": 0.1}]

    monkeypatch.setattr(healthcheck, "run", fake_run)
    engine = MagnetEngine(
        registry={"sites": [{"id": "rtings", "domain": "rtings.com", "language": "en"}]},
        index_path=str(tmp_path / "missing.sqlite"),
    )
    report = asyncio.run(engine.health(probe=True))
    assert report["ok"] is True
    assert report["probe"] == [{"site": "rtings", "status": "ok", "links": 3,
                               "circuit_open": False, "secs": 0.1}]
    assert report["index"]["missing"] is True


def test_bad_arguments_are_tool_errors():
    async def run(session, init):
        cases = [
            ("magnet_search", {"query": "  "}, "non-empty"),
            ("magnet_search", {"query": "lg c1", "k": 0}, "k must be"),
            ("magnet_search", {"query": "lg c1", "k": 11}, "k must be"),
            ("magnet_search", {"query": "lg c1", "lang": "de"}, "lang must be"),
            ("magnet_fetch", {"url": "example.com"}, "http(s)"),
            ("magnet_fetch", {"url": "ftp://example.com/x"}, "http(s)"),
        ]
        outcomes = []
        for name, arguments, needle in cases:
            result, text = await _call(session, name, arguments)
            outcomes.append((arguments, result.is_error, text, needle))
        return outcomes

    outcomes = _run(run)
    for arguments, is_error, text, needle in outcomes:
        assert is_error is True, (arguments, text)
        assert needle in text, (arguments, text)


def test_missing_and_unknown_arguments_fail():
    async def run(session, init):
        missing, text = await _call(session, "magnet_fetch", {})
        unknown, unknown_text = await _call(session, "magnet_nope", {"query": "x"})
        return (missing, text), (unknown, unknown_text)

    (missing, text), (unknown, unknown_text) = _run(run)
    assert missing.is_error is True
    assert "url" in text
    assert unknown.is_error is True
    assert "magnet_nope" in unknown_text
