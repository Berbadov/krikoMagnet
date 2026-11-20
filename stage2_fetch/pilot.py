import asyncio
import sys
sys.path.insert(0, ".")
from curl_cffi import requests as cr

UA = "KrikoMagnet/0.2 (+https://kriko.cc/bot)"

URLS = [
    "https://www.rtings.com/tv/reviews",
    "https://www.theverge.com/",
    "https://en.wikipedia.org/wiki/Mechanical_keyboard",
    "https://stackoverflow.com/questions/1234",
    "https://support.apple.com/en-us/HT210456",
    "https://www.reddit.com/r/apple/",
    "https://www.ifixit.com/Guide/iPhone+14+Battery+Replacement/151677",
    "https://docs.python.org/3/library/asyncio.html",
    "https://www.healthline.com/nutrition/intermittent-fasting-guide",
    "https://www.gsmarena.com/best_camera_phones-review-2712.php",
    "https://www.wta.org/hikes/browse",
    "https://www.alltrails.com/parks/us/washington/seattle",
]


async def fetch(url, sem):
    async with sem:
        try:
            async with cr.AsyncSession() as s:
                r = await s.get(url, timeout=25, allow_redirects=True, headers={"User-Agent": UA})
                return url, r.status_code, len(r.content), r.headers.get("server", "")[:30]
        except Exception as e:
            return url, "ERR", 0, repr(e)[:50]


async def main():
    sem = asyncio.Semaphore(4)
    res = await asyncio.gather(*[fetch(u, sem) for u in URLS])
    for r in res:
        print(f"{r[0]:65s} status={r[1]} bytes={r[2]:>8} server={r[3]}")


asyncio.run(main())
