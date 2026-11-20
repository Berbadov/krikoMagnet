"""Magnet side of the extraction benchmark: cached markdown + timed trafilatura.

Reads the copies of Magnet's eval caches in this worktree (data/), never the
live ones.
"""
from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
WT = os.path.dirname(HERE)
sys.path.insert(0, WT)

from fetchlib.extract import extract_text  # noqa: E402
from fetchlib.http import decode_body  # noqa: E402

from extract_bench import pick_urls  # noqa: E402


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", required=True)
    ap.add_argument("--limit", type=int, default=100)
    ap.add_argument("--out", default=os.path.join(HERE, "out", "magnet.jsonl"))
    args = ap.parse_args()

    urls = pick_urls(args.results, args.limit)
    cache = sqlite3.connect("file:" + os.path.join(WT, "data", "eval_cache.sqlite").replace("\\", "/") + "?mode=ro", uri=True)
    fetch = sqlite3.connect("file:" + os.path.join(WT, "data", "bench_fetch.sqlite").replace("\\", "/") + "?mode=ro", uri=True)
    md_by_url = {u: m for u, m in cache.execute("SELECT url, markdown FROM cache")}
    body_by_url = {}
    hdr_by_url = {}
    for u, h, b in fetch.execute("SELECT url, headers, body FROM http_cache"):
        body_by_url[u] = b
        try:
            hdr_by_url[u] = json.loads(h) or {}
        except Exception:
            hdr_by_url[u] = {}

    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    with open(args.out, "w", encoding="utf-8") as f:
        for i, u in enumerate(urls, 1):
            md = md_by_url.get(u)
            html = None
            if u in body_by_url:
                html = decode_body(body_by_url[u], hdr_by_url[u])
            rec = {"url": u, "cached_markdown": md is not None, "in_fetch_cache": html is not None}
            if html:
                t0 = time.perf_counter()
                text = extract_text(html)
                rec["extract_ms"] = (time.perf_counter() - t0) * 1000
                rec["extract_chars"] = len(text or "")
                path = os.path.join(os.path.dirname(args.out), "mag", str(abs(hash(u))) + ".txt")
                os.makedirs(os.path.dirname(path), exist_ok=True)
                open(path, "w", encoding="utf-8").write(text or "")
                rec["path"] = os.path.basename(path)
                rec["html_chars"] = len(html)
            if md is not None:
                rec["cached_markdown_chars"] = len(md)
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
            print(f"[{i:03d}/{len(urls)}] html={bool(html)} md={md is not None} {u[:80]}", flush=True)


if __name__ == "__main__":
    main()
