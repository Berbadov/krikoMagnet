"""Dump side-by-side previews of ~15 hand-checked pages (one per host)."""
from __future__ import annotations

import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


def read(base, name):
    if not name:
        return ""
    p = os.path.join(HERE, "out", base, name)
    return open(p, encoding="utf-8").read() if os.path.exists(p) else ""


def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 15
    rows = [json.loads(l) for l in open(os.path.join(HERE, "out", "extract.jsonl"), encoding="utf-8") if l.strip()]
    mag = {json.loads(l)["url"]: json.loads(l) for l in open(os.path.join(HERE, "out", "magnet.jsonl"), encoding="utf-8") if l.strip()}
    seen_host, picked = set(), []
    for r in rows:
        if r["host"] in seen_host or not r.get("firecrawl", {}).get("ok"):
            continue
        seen_host.add(r["host"])
        picked.append(r)
        if len(picked) >= n:
            break
    for i, r in enumerate(picked, 1):
        m = mag.get(r["url"], {})
        fc = read("fc", r["firecrawl"].get("path"))
        mt = read("mag", m.get("path"))
        print("=" * 100)
        print(f"#{i} {r['url']}")
        print(f"host={r['host']} fc_chars={len(fc)} mag_chars={len(mt)}")
        print("-" * 40, "FIRECRAWL head")
        print(fc[:900].replace("\n", " | "))
        print("-" * 40, "MAGNET head")
        print(mt[:900].replace("\n", " | "))
        print("-" * 40, "FIRECRAWL tail")
        print(fc[-400:].replace("\n", " | "))
        print("-" * 40, "MAGNET tail")
        print(mt[-400:].replace("\n", " | "))


if __name__ == "__main__":
    main()
