"""Load and validate the site registry."""
from __future__ import annotations

import os

import yaml

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_PATH = os.path.join(HERE, "registry.yaml")
DATA_DIR = os.path.join(HERE, "..", "data")


def load_registry(path: str = DEFAULT_PATH) -> dict:
    with open(path, encoding="utf-8") as f:
        data = yaml.safe_load(f)
    if not isinstance(data, dict) or not data.get("sites"):
        raise ValueError(f"registry has no sites: {path}")
    seen = set()
    for site in data["sites"]:
        for key in ("id", "domain", "topics", "language", "methods"):
            if key not in site:
                raise ValueError(f"site {site.get('id', '?')} missing {key}")
        if site["id"] in seen:
            raise ValueError(f"duplicate site id: {site['id']}")
        seen.add(site["id"])
        if not isinstance(site["topics"], list) or not site["topics"]:
            raise ValueError(f"site {site['id']} has no topics")
        if not isinstance(site["methods"], list) or not site["methods"]:
            raise ValueError(f"site {site['id']} has no methods")
        for method in site["methods"]:
            if method.get("type") not in ("search", "sitemap", "rss", "api", "url_pattern"):
                raise ValueError(f"site {site['id']} has unknown method {method.get('type')}")
    return data


_TRANCO_CACHE: dict[str, tuple[float, dict[str, int]]] = {}


def load_tranco_ranks(path: str | None = None) -> dict[str, int]:
    path = path or os.path.join(DATA_DIR, "tranco_top100k.txt")
    try:
        mtime = os.path.getmtime(path)
    except OSError:
        return {}
    hit = _TRANCO_CACHE.get(path)
    if hit is not None and hit[0] == mtime:
        return hit[1]
    ranks = {}
    with open(path, encoding="utf-8") as f:
        for i, line in enumerate(f):
            domain = line.strip().lower()
            if domain and domain not in ranks:
                ranks[domain] = i + 1
    _TRANCO_CACHE[path] = (mtime, ranks)
    return ranks
