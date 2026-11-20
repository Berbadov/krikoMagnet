"""Quality + size metrics for the extraction-stage comparison.

Outputs a per-URL table and aggregate numbers. Reference text = the page's own
<main>/<article> element from the raw HTML Magnet already cached (fallback:
trafilatura precision mode), so recall is measured against the page structure,
not against either extractor's output.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import re
import sqlite3
import statistics
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
WT = os.path.dirname(HERE)
sys.path.insert(0, WT)

from selectolax.lexbor import LexborHTMLParser  # noqa: E402
from fetchlib.http import decode_body  # noqa: E402

BOILER = re.compile(
    r"(cookie|çerez|newsletter|bülten|subscribe|abone|sign in|log in|giriş yap|"
    r"share this|paylaş|advertisement|reklam|related articles|ilgili haber|"
    r"skip to (main )?content|ana içeriğe geç|accept all|tümünü kabul|"
    r"follow us|bizi takip|download the app|uygulamayı indir|privacy policy|gizlilik)",
    re.I,
)
WS = re.compile(r"\s+")
WORD = re.compile(r"[0-9a-zA-ZçğıöşüÇĞİÖŞÜâîû]+", re.U)


def norm_tokens(text):
    return WORD.findall((text or "").lower())


def shingles(tokens, n=8):
    return {" ".join(tokens[i : i + n]) for i in range(0, max(0, len(tokens) - n + 1))}


def reference_text(html):
    if not html:
        return ""
    try:
        tree = LexborHTMLParser(html)
    except Exception:
        return ""
    best = ""
    for sel in ("article", "main", "[role=main]", "div.post-content", "div.entry-content"):
        for node in tree.css(sel):
            t = WS.sub(" ", node.text(separator=" ")).strip()
            if len(t) > len(best):
                best = t
    return best


def load_jsonl(path):
    out = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                out.append(json.loads(line))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--fc", default=os.path.join(HERE, "out", "extract.jsonl"))
    ap.add_argument("--mag", default=os.path.join(HERE, "out", "magnet.jsonl"))
    ap.add_argument("--out", default=os.path.join(HERE, "out", "quality.json"))
    args = ap.parse_args()

    fc = {r["url"]: r for r in load_jsonl(args.fc)}
    mag = {r["url"]: r for r in load_jsonl(args.mag)}
    fetch = sqlite3.connect("file:" + os.path.join(WT, "data", "bench_fetch.sqlite").replace("\\", "/") + "?mode=ro", uri=True)
    bodies = {}
    for u, h, b in fetch.execute("SELECT url, headers, body FROM http_cache"):
        try:
            hdrs = json.loads(h) or {}
        except Exception:
            hdrs = {}
        bodies[u] = decode_body(b, hdrs)

    def read(base, name):
        if not name:
            return ""
        p = os.path.join(HERE, "out", base, name)
        return open(p, encoding="utf-8").read() if os.path.exists(p) else ""

    rows = []
    for url in sorted(set(fc) | set(mag)):
        f = fc.get(url, {})
        m = mag.get(url, {})
        fcr = f.get("firecrawl", {})
        fc_text = read("fc", fcr.pop("path", None)) if fcr.get("ok") else ""
        mag_text = read("mag", m.get("path"))
        ref = reference_text(bodies.get(url))
        ref_src = "structural" if ref else "none"
        if not ref:
            ref = mag_text
        if len(ref) < 500:
            ref = ref if ref else ""
        ref_sh = shingles(norm_tokens(ref))
        def stats(text):
            toks = norm_tokens(text)
            sh = shingles(toks)
            rec = len(sh & ref_sh) / len(ref_sh) if ref_sh else 0.0
            prec = len(sh & ref_sh) / len(sh) if sh else 0.0
            f1 = 2 * rec * prec / (rec + prec) if rec + prec else 0.0
            lines = [l.strip() for l in text.splitlines() if l.strip()]
            boiler = sum(1 for l in lines if BOILER.search(l))
            links = re.findall(r"\]\(http", text)
            return {
                "chars": len(text),
                "words": len(toks),
                "recall": rec,
                "precision": prec,
                "f1": f1,
                "boiler_lines": boiler,
                "line_count": len(lines),
                "boiler_share": boiler / len(lines) if lines else 0.0,
                "links_per_1k": len(links) * 1000 / len(text) if text else 0.0,
            }
        fs, ms = stats(fc_text), stats(mag_text)
        rows.append({
            "url": url,
            "host": f.get("host", ""),
            "ref_chars": len(ref),
            "ref_src": ref_src,
            "robots_denied": f.get("robots_denied", False),
            "fc_ok": bool(fcr.get("ok")),
            "fc_http": fcr.get("http"),
            "fc_status": fcr.get("statusCode"),
            "fc_error": (fcr.get("error") or "")[:160],
            "fc_ms": fcr.get("ms"),
            "fc": fs,
            "mag": ms,
        })

    ok = [r for r in rows if r["fc_ok"] and not r["robots_denied"]]
    denied = [r for r in rows if r["robots_denied"]]
    failed = [r for r in rows if not r["fc_ok"] and not r["robots_denied"]]
    paired = [r for r in ok if r["ref_src"] == "structural" and r["ref_chars"] >= 500 and r["mag"]["chars"] > 0]
    ms = sorted(r["fc_ms"] for r in ok if r["fc_ms"])
    mag_ms = sorted(m["extract_ms"] for m in mag.values() if m.get("extract_ms"))
    def pct(v, p):
        return v[min(len(v) - 1, int(len(v) * p))] if v else 0

    summary = {
        "urls": len(rows),
        "robots_denied": len(denied),
        "attempted": len(rows) - len(denied),
        "fc_success": len(ok),
        "fc_success_rate": len(ok) / max(1, len(rows) - len(denied)),
        "fc_failed": len(failed),
        "fc_latency_p50_ms": statistics.median(ms) if ms else 0,
        "fc_latency_p95_ms": pct(ms, 0.95),
        "mag_extract_p50_ms": statistics.median(mag_ms) if mag_ms else 0,
        "mag_extract_p95_ms": pct(mag_ms, 0.95),
        "fc_chars_mean": statistics.mean(r["fc"]["chars"] for r in ok) if ok else 0,
        "mag_chars_mean": statistics.mean(r["mag"]["chars"] for r in ok if r["mag"]["chars"]) if ok else 0,
        "paired_n": len(paired),
        "paired_ref_chars_mean": statistics.mean(r["ref_chars"] for r in paired) if paired else 0,
        "paired_fc_chars_mean": statistics.mean(r["fc"]["chars"] for r in paired) if paired else 0,
        "paired_mag_chars_mean": statistics.mean(r["mag"]["chars"] for r in paired) if paired else 0,
        "paired_fc_recall": statistics.mean(r["fc"]["recall"] for r in paired) if paired else 0,
        "paired_mag_recall": statistics.mean(r["mag"]["recall"] for r in paired) if paired else 0,
        "paired_fc_f1": statistics.mean(r["fc"]["f1"] for r in paired) if paired else 0,
        "paired_mag_f1": statistics.mean(r["mag"]["f1"] for r in paired) if paired else 0,
        "paired_fc_boiler": statistics.mean(r["fc"]["boiler_share"] for r in paired) if paired else 0,
        "paired_mag_boiler": statistics.mean(r["mag"]["boiler_share"] for r in paired) if paired else 0,
        "paired_fc_wins": sum(1 for r in paired if r["fc"]["f1"] > r["mag"]["f1"] + 0.02),
        "paired_mag_wins": sum(1 for r in paired if r["mag"]["f1"] > r["fc"]["f1"] + 0.02),
        "fc_missing_text": sum(1 for r in ok if r["fc"]["chars"] < 300),
        "mag_missing_text": sum(1 for r in ok if r["mag"]["chars"] < 300),
        "fc_recall_mean": statistics.mean(r["fc"]["recall"] for r in ok) if ok else 0,
        "mag_recall_mean": statistics.mean(r["mag"]["recall"] for r in ok if r["mag"]["chars"]) if ok else 0,
        "fc_f1_mean": statistics.mean(r["fc"]["f1"] for r in ok) if ok else 0,
        "mag_f1_mean": statistics.mean(r["mag"]["f1"] for r in ok if r["mag"]["chars"]) if ok else 0,
        "fc_boiler_share": statistics.mean(r["fc"]["boiler_share"] for r in ok) if ok else 0,
        "mag_boiler_share": statistics.mean(r["mag"]["boiler_share"] for r in ok if r["mag"]["chars"]) if ok else 0,
        "fc_links_per_1k": statistics.mean(r["fc"]["links_per_1k"] for r in ok) if ok else 0,
        "mag_links_per_1k": statistics.mean(r["mag"]["links_per_1k"] for r in ok if r["mag"]["chars"]) if ok else 0,
        "short_fc": sum(1 for r in ok if r["fc"]["chars"] < 300),
        "short_mag": sum(1 for r in ok if r["mag"]["chars"] and r["mag"]["chars"] < 300),
        "failed": [{"url": r["url"], "http": r["fc_http"], "error": r["fc_error"]} for r in failed],
        "denied": [r["url"] for r in denied],
    }
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump({"summary": summary, "rows": rows}, f, ensure_ascii=False, indent=1)
    print(json.dumps({k: v for k, v in summary.items() if k not in ("failed", "denied")}, indent=1))
    print("failed:", len(failed), "denied:", len(denied))


if __name__ == "__main__":
    main()
