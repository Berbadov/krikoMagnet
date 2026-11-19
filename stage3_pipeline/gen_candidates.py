"""Generate candidates.jsonl (scoring metadata for real target URLs) and
queries_pipeline.json (10 end-to-end benchmark queries).
"""
import json
import os
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
NOW = datetime.now(timezone.utc)


def days_ago(d):
    return (NOW - timedelta(days=d)).isoformat()


C = {
    "c01": {"url": "https://www.gsmarena.com/", "domain": "gsmarena.com",
             "title": "GSMArena - mobile phone reviews and specs",
             "snippet": "Smartphone specs, reviews, and camera tests. Best camera phones ranked by photo and video quality and battery benchmarks.",
             "published": days_ago(5)},
    "c02": {"url": "https://www.dxomark.com/smartphones/", "domain": "dxomark.com",
             "title": "Best smartphone cameras - DxOMark",
             "snippet": "DxOMark ranks smartphone cameras with measured photo, video, and zoom sub-scores for the latest Android flagships.",
             "published": days_ago(12)},
    "c03": {"url": "https://www.theverge.com/", "domain": "theverge.com",
             "title": "The Verge - technology news and reviews",
             "snippet": "Technology news and hands-on reviews of consumer electronics, Android smartphones, and gadgets.",
             "published": days_ago(2)},
    "c04": {"url": "https://www.wired.com/", "domain": "wired.com",
             "title": "WIRED - gear and gadget reviews",
             "snippet": "Gadget reviews, tech culture, and deep dives into consumer electronics and smartphones.",
             "published": days_ago(9)},
    "c05": {"url": "https://www.zdnet.com/", "domain": "zdnet.com",
             "title": "ZDNET - technology product reviews",
             "snippet": "Business technology and consumer electronics reviews, including phones, laptops, and software.",
             "published": days_ago(15)},
    "c06": {"url": "https://sqlite.org/windowfunctions.html", "domain": "sqlite.org",
             "title": "SQLite window functions - official documentation",
             "snippet": "Official SQLite reference for window functions: OVER, PARTITION BY, ORDER BY, frames, and built-in window functions.",
             "published": days_ago(20)},
    "c07": {"url": "https://stackexchange.com/", "domain": "stackexchange.com",
             "title": "Stack Exchange - Q&A network",
             "snippet": "Question and answer community covering programming, databases, and technical topics with accepted answers.",
             "published": days_ago(30)},
    "c08": {"url": "https://nginx.org/en/docs/beginners_guide.html", "domain": "nginx.org",
             "title": "NGINX beginners guide - official docs",
             "snippet": "Official NGINX guide covering configuration basics, server blocks, and location directive matching.",
             "published": days_ago(40)},
    "c09": {"url": "https://docs.python.org/3/library/asyncio.html", "domain": "python.org",
             "title": "asyncio - Asynchronous I/O - Python docs",
             "snippet": "Python standard library documentation for asyncio: coroutines, tasks, gather, and event loops for concurrent I/O.",
             "published": days_ago(25)},
    "c10": {"url": "https://www.postgresql.org/docs/current/indexes-partial.html", "domain": "postgresql.org",
             "title": "PostgreSQL partial indexes - official docs",
             "snippet": "Official PostgreSQL documentation for partial indexes and when a WHERE clause index accelerates subset queries.",
             "published": days_ago(18)},
    "c11": {"url": "https://developer.mozilla.org/en-US/docs/Web/JavaScript/Guide", "domain": "mozilla.org",
             "title": "JavaScript Guide - MDN Web Docs",
             "snippet": "MDN JavaScript guide covering language fundamentals, including asynchronous programming and web platform concepts.",
             "published": days_ago(35)},
    "c12": {"url": "https://docs.docker.com/get-started/", "domain": "docker.com",
             "title": "Docker get started - official docs",
             "snippet": "Docker official getting-started guide covering containers, images, and Compose basics.",
             "published": days_ago(22)},
    "c13": {"url": "https://realpython.com/async-io-python/", "domain": "realpython.com",
             "title": "Async IO in Python - Real Python",
             "snippet": "Real Python guide to asyncio: writing concurrent HTTP requests, tasks, event loops, and pitfalls.",
             "published": days_ago(14)},
    "c14": {"url": "https://www.tomshardware.com/pc-components/gpus", "domain": "tomshardware.com",
             "title": "Tom's Hardware - GPU and PC component reviews",
             "snippet": "PC hardware reviews including GPUs, cooling noise, thermals, and product comparisons.",
             "published": days_ago(8)},
    "c15": {"url": "https://www.rtings.com/tv/reviews", "domain": "rtings.com",
             "title": "RTINGS TV reviews - display testing",
             "snippet": "Objectively tested TVs and monitors by brightness, color accuracy, contrast, and viewing angle.",
             "published": days_ago(11)},
    "c16": {"url": "https://www.healthline.com/nutrition/intermittent-fasting-guide", "domain": "healthline.com",
             "title": "Intermittent fasting 101 - Healthline",
             "snippet": "Intermittent fasting guide: methods, benefits, and how eating windows affect metabolism.",
             "published": days_ago(28)},
    "c17": {"url": "https://support.apple.com/", "domain": "apple.com",
             "title": "Apple Support",
             "snippet": "Apple product support: manuals, warranty info, and troubleshooting help for devices.",
             "published": days_ago(1)},
    "c18": {"url": "https://support.microsoft.com/en-us/windows", "domain": "microsoft.com",
             "title": "Microsoft Windows Support",
             "snippet": "Windows support: how-to articles, driver help, and warranty/service information.",
             "published": days_ago(3)},
    "c19": {"url": "https://support.hp.com/us-en", "domain": "hp.com",
             "title": "HP Support",
             "snippet": "HP support: drivers, troubleshooting, warranty claims, and service center lookup.",
             "published": days_ago(6)},
    "c20": {"url": "https://www.lenovo.com/us/en/", "domain": "lenovo.com",
             "title": "Lenovo - laptops, desktops and support",
             "snippet": "Lenovo product pages and support for laptops and desktops, including warranty coverage.",
             "published": days_ago(10)},
    "c21": {"url": "https://forums.tomshardware.com/", "domain": "tomshardware.com",
             "title": "Tom's Hardware Forums",
             "snippet": "Community forums discussing PC builds, components, and hardware troubleshooting.",
             "published": days_ago(7)},
    "c22": {"url": "https://www.macrumors.com/", "domain": "macrumors.com",
             "title": "MacRumors - Apple news and rumors",
             "snippet": "Apple news, rumors, and discussion boards about iPhones, iPads, and Macs.",
             "published": days_ago(4)},
    "c23": {"url": "https://arstechnica.com/", "domain": "arstechnica.com",
             "title": "Ars Technica - technology and science",
             "snippet": "In-depth technology, science, and gadget reviews and reporting.",
             "published": days_ago(16)},
    "c24": {"url": "https://www.reddit.com/r/Android/", "domain": "reddit.com",
             "title": "r/Android - Reddit",
             "snippet": "Reddit community discussions about Android phones, cameras, and battery life.",
             "published": days_ago(2)},
    "c25": {"url": "https://www.quora.com/", "domain": "quora.com",
             "title": "Quora - questions and answers",
             "snippet": "Community Q&A covering technology, product advice, and general knowledge.",
             "published": days_ago(13)},
    "c26": {"url": "https://www.sitepoint.com/community/", "domain": "sitepoint.com",
             "title": "SitePoint Forums - web development",
             "snippet": "Web development community forums covering CSS, JavaScript, and web standards.",
             "published": days_ago(19)},
}

QUERIES = [
    {"id": "pq1", "text": "best android phone camera 2026",
     "candidates": ["c01", "c02", "c03", "c04", "c05", "c24"]},
    {"id": "pq2", "text": "sqlite window functions tutorial",
     "candidates": ["c06", "c07", "c08", "c09", "c26"]},
    {"id": "pq3", "text": "postgresql jsonb partial index",
     "candidates": ["c10", "c07", "c11", "c06"]},
    {"id": "pq4", "text": "python asyncio httpx concurrent requests",
     "candidates": ["c09", "c13", "c07", "c11"]},
    {"id": "pq5", "text": "nginx location regex priority",
     "candidates": ["c08", "c07", "c11", "c26"]},
    {"id": "pq6", "text": "home espresso machine review under $500",
     "candidates": ["c14", "c03", "c04", "c05", "c22"]},
    {"id": "pq7", "text": "smartphone battery life comparison",
     "candidates": ["c01", "c02", "c03", "c04", "c24"]},
    {"id": "pq8", "text": "laptop display brightness and color accuracy",
     "candidates": ["c15", "c14", "c03", "c04", "c20"]},
    {"id": "pq9", "text": "how https tls encryption works",
     "candidates": ["c11", "c07", "c12", "c26"]},
    {"id": "pq10", "text": "laptop manufacturer warranty support",
     "candidates": ["c17", "c18", "c19", "c20", "c05"]},
]


def write():
    with open(os.path.join(HERE, "candidates.jsonl"), "w", encoding="utf-8") as f:
        for cid, meta in C.items():
            rec = {"id": cid, "url": meta["url"], "domain": meta["domain"],
                   "title": meta["title"], "snippet": meta["snippet"],
                   "published": meta["published"]}
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
    with open(os.path.join(HERE, "queries_pipeline.json"), "w", encoding="utf-8") as f:
        json.dump(QUERIES, f, ensure_ascii=False, indent=2)
    print(f"candidates: {len(C)}  queries: {len(QUERIES)}")
    for q in QUERIES:
        print(f"  {q['id']} {q['text'][:40]:40s} candidates={len(q['candidates'])}")


if __name__ == "__main__":
    write()
