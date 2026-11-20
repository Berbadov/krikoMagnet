"""Kriko Magnet — local stdio MCP server.

Run with the repo venv:
    .venv\\Scripts\\python.exe mcp_server\\server.py      (or: python -m mcp_server)

stdout carries the MCP protocol; all logging goes to stderr.
"""
from __future__ import annotations

import importlib
import logging
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
if ROOT not in sys.path:
    sys.path.insert(0, ROOT)

from mcp.server.mcpserver import MCPServer

from mcp_server.engine import MagnetEngine

INSTRUCTIONS = (
    "Kriko Magnet searches a curated registry of high-value review, complaint, "
    "recall and repair sites without a search-engine provider, then extracts the "
    "page text politely (honest user agent, robots.txt, >=1s per host). "
    "Use magnet_search for product/buying questions (reliability, burn-in, faults, "
    "recalls, repair); it is slower than web search but cites real pages. "
    "Use magnet_fetch to read one page; robots-disallowed URLs are refused. "
    "Use magnet_health when results look empty or stale."
)

mcp = MCPServer("magnet", version="0.1.0", instructions=INSTRUCTIONS)
_ENGINE = None


def get_engine() -> MagnetEngine:
    global _ENGINE
    if _ENGINE is None:
        spec = os.environ.get("MAGNET_MCP_ENGINE", "").strip()
        if spec:
            module_name, _, attr = spec.partition(":")
            module = importlib.import_module(module_name)
            _ENGINE = getattr(module, attr or "make_engine")()
        else:
            _ENGINE = MagnetEngine()
    return _ENGINE


@mcp.tool()
async def magnet_search(query: str, k: int = 5, lang: str | None = None,
                        fetch: bool = True) -> dict:
    """Rank pages for a query from the Kriko Magnet site registry.

    Args:
        query: Natural-language query, e.g. "lg c1 burn in" or "iphone 15 battery".
        k: Number of results to return (1-10).
        lang: Optional language hint, "en" or "tr". Results from sites tagged for
            that language are preferred; others are kept only if none match.
        fetch: Fetch and extract the top pages (default). False returns ranked
            links with discovery snippets only, which is much faster.
    """
    return await get_engine().search(query, k=k, lang=lang, fetch=fetch)


@mcp.tool()
async def magnet_fetch(url: str) -> dict:
    """Fetch one URL through the polite layer and return its extracted main text.

    Refuses robots.txt-disallowed URLs and reports the refusal instead of fetching.
    """
    return await get_engine().fetch(url)


@mcp.tool()
async def magnet_health(probe: bool = False) -> dict:
    """Adapter health report: registry, local index, resident ranker, open circuits.

    Args:
        probe: Also run one live query per site (slow, up to ~30s). Off by default.
    """
    return await get_engine().health(probe=probe)


def main() -> None:
    logging.basicConfig(
        level=os.environ.get("KRIKO_LOG", "WARNING").upper(),
        stream=sys.stderr,
        format="%(asctime)s %(levelname)s %(name)s %(message)s",
    )
    mcp.run("stdio")


if __name__ == "__main__":
    main()
