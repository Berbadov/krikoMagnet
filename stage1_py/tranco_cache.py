"""Tranco Top-1M domain list download/cache.

Downloads the free daily Tranco list via curl_cffi (browser TLS
impersonation to bypass Cloudflare SSL challenges) and caches it on disk.
Provides a mapping domain -> (rank, log_prior) for ranking priors.
"""
import io
import os
import sys
import csv
import zipfile

import requests as _
from curl_cffi import requests as cr

DATA_DIR = os.path.join(os.path.dirname(__file__), "..", "data")
ZIP_PATH = os.path.join(DATA_DIR, "tranco_top-1m.csv.zip")
TOP1K_PATH = os.path.join(DATA_DIR, "tranco_top100k.txt")

URL = "https://tranco-list.s3.amazonaws.com/top-1m.csv.zip"


def download_zip(force: bool = False) -> str:
    """Download and cache the Tranco zip. Returns path to the cached zip."""
    os.makedirs(DATA_DIR, exist_ok=True)
    if os.path.exists(ZIP_PATH) and os.path.getsize(ZIP_PATH) > 1_000_000 and not force:
        return ZIP_PATH
    for attempt in range(4):
        try:
            r = cr.get(URL, impersonate="chrome", timeout=120)
            if r.status_code == 200 and r.content[:2] == b"PK":
                with open(ZIP_PATH, "wb") as f:
                    f.write(r.content)
                return ZIP_PATH
        except Exception as e:  # network hiccup
            print(f"attempt {attempt} failed: {e}", file=sys.stderr)
    raise RuntimeError("could not download Tranco list")


def extract_top(limit: int = 100_000) -> list[str]:
    """Return first `limit` domains in ranking order."""
    zip_path = download_zip()
    with open(zip_path, "rb") as f:
        data = f.read()
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        name = z.namelist()[0]
        with z.open(name) as raw:
            reader = csv.reader(io.TextIOWrapper(raw, encoding="utf-8"))
            out = []
            for row in reader:
                if not row or len(row) < 2:
                    continue
                out.append(row[1])
                if len(out) >= limit:
                    break
    return out


def cache_top100k(force: bool = False) -> str:
    """Write top-100k domains to a plaintext cache file."""
    if os.path.exists(TOP1K_PATH) and not force:
        return TOP1K_PATH
    domains = extract_top(100_000)
    with open(TOP1K_PATH, "w", encoding="utf-8") as f:
        f.write("\n".join(domains))
    return TOP1K_PATH


def load_top100k() -> list[str]:
    cache_top100k()
    with open(TOP1K_PATH, "r", encoding="utf-8") as f:
        return [line.strip() for line in f if line.strip()]


if __name__ == "__main__":
    p = cache_top100k()
    doms = load_top100k()
    print("cached:", p)
    print("count:", len(doms))
    print("top5:", doms[:5])
