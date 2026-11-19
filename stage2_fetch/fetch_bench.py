"""Stage 2: direct website fetching & block-rate telemetry.

Fetches 40 real URLs concurrently (concurrency=8) with curl_cffi
(impersonate="chrome"), extracts clean main-body text with trafilatura
(selectolax fallback), and prints a per-domain telemetry table.
"""
import asyncio
import json
import time
import sys

from curl_cffi import requests as cr

# Challenge markers matched against the *extracted clean text* only (avoids
# false positives from JavaScript payloads that merely mention these words).
TEXT_CHALLENGE_MARKERS = [
    "just a moment", "enable javascript and cookies", "verify you are human",
    "captcha", "attention required", "access denied", "checking your browser",
]
# Specific Cloudflare/Turnstile infrastructure tokens scanned in raw HTML.
HTML_CHALLENGE_TOKENS = [
    "cf-chl", "cf-browser-verification", "challenge-platform", "turnstile",
    "challenges.cloudflare.com", "cloudflare-challenge",
]
SOFT_MIN_CHARS = 300
CONCURRENCY = 8


def extract_text(html: str) -> str:
    text = None
    try:
        import trafilatura
        text = trafilatura.extract(html, include_comments=False, include_tables=True, favor_recall=True)
    except Exception:
        text = None
    if text and len(text) >= SOFT_MIN_CHARS:
        return text
    # selectolax fallback
    try:
        from selectolax.parser import HTMLParser
        tree = HTMLParser(html)
        body = tree.body
        text = body.text(separator=" ") if body is not None else ""
        text = " ".join(text.split())
    except Exception:
        text = text or ""
    return text


async def fetch_one(session, entry):
    url = entry["url"]
    t0 = time.perf_counter()
    try:
        r = await session.get(url, timeout=25, allow_redirects=True)
        status = r.status_code
        html = r.text
        fetch_ms = (time.perf_counter() - t0) * 1000
        body = r.content[:200000].decode("utf-8", errors="ignore")
    except Exception as e:
        return {**entry, "status": "ERR", "fetch_ms": (time.perf_counter() - t0) * 1000,
                "clean_chars": 0, "marker": False, "verdict": "network-error", "err": repr(e)[:80]}

    t1 = time.perf_counter()
    text = extract_text(html)
    parse_ms = (time.perf_counter() - t1) * 1000
    clean = len(text)
    text_low = text.lower()
    marker = any(m in text_low for m in TEXT_CHALLENGE_MARKERS)

    if status in (403, 429, 401):
        verdict = "hard-block"
    elif status != 200:
        verdict = "http-error"
    elif clean < SOFT_MIN_CHARS or marker:
        verdict = "soft-block"
    else:
        verdict = "success"

    return {
        **entry, "status": status, "fetch_ms": fetch_ms, "parse_ms": parse_ms,
        "clean_chars": clean, "marker": marker, "verdict": verdict,
    }


async def main():
    with open("urls.json", encoding="utf-8") as f:
        entries = json.load(f)
    sem = asyncio.Semaphore(CONCURRENCY)
    results = []

    async def worker(e):
        async with sem:
            async with cr.AsyncSession(impersonate="chrome") as s:
                res = await fetch_one(s, e)
        results.append(res)

    await asyncio.gather(*[worker(e) for e in entries])

    results.sort(key=lambda x: (x["verdict"], x["url"]))
    from urllib.parse import urlparse
    print("=" * 118)
    print(f"{'domain':<28}{'cat':<20}{'status':<7}{'clean_chars':<12}{'fetch_ms':<9}{'parse_ms':<9}{'verdict'}")
    print("-" * 118)
    for r in results:
        dom = urlparse(r["url"]).netloc
        print(f"{dom:<28}{r['category']:<20}{str(r['status']):<7}{r['clean_chars']:<12}{r['fetch_ms']:<9.0f}{r['parse_ms']:<9.1f}{r['verdict']}")
    print("=" * 118)

    n = len(results)
    succ = [r for r in results if r["verdict"] == "success"]
    hard = [r for r in results if r["verdict"] == "hard-block"]
    soft = [r for r in results if r["verdict"] == "soft-block"]
    http = [r for r in results if r["verdict"] == "http-error"]
    neterr = [r for r in results if r["verdict"] == "network-error"]

    success_rate = len(succ) / n
    block_rate = (len(hard) + len(soft)) / n
    print(f"\nTOTAL targets: {n}")
    print(f"clean success: {len(succ)}  ({success_rate*100:.1f}%)")
    print(f"hard blocks:   {len(hard)}   soft blocks: {len(soft)}   total blocks: {len(hard)+len(soft)} ({block_rate*100:.1f}%)")
    print(f"http errors:   {len(http)}   network errors: {len(neterr)}")

    if hard:
        from urllib.parse import urlparse
        print("\nBLOCKED domains/signatures:")
        for r in hard:
            print(f"  - {urlparse(r['url']).netloc:<24} status={r['status']} (hard)")
        for r in soft:
            print(f"  - {urlparse(r['url']).netloc:<24} chars={r['clean_chars']} marker={'yes' if r['marker'] else 'no'} (soft)")

    print("\n" + "=" * 60)
    print(f"GATE 2: success_rate={success_rate*100:.1f}% (need >=75%), block_rate={block_rate*100:.1f}% (need <=25%)")
    if success_rate >= 0.75 and block_rate <= 0.25:
        print("GATE 2: PASS")
    else:
        print("GATE 2: FAIL -> STOP, list blocked domains, do not proceed to Stage 3")


if __name__ == "__main__":
    asyncio.run(main())
