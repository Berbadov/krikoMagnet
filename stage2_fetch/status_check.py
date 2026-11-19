import asyncio
from curl_cffi import requests as cr

URLS = [
    "https://arstechnica.com/gadgets/",
    "https://www.wired.com/",
    "https://techcrunch.com/",
    "https://apnews.com/",
    "https://www.cnn.com/world",
    "https://www.aljazeera.com/",
    "https://www.macrumors.com/",
    "https://www.zdnet.com/",
    "https://support.apple.com/",
    "https://support.microsoft.com/en-us/windows",
    "https://support.hp.com/us-en",
    "https://www.lenovo.com/us/en/support/",
    "https://support.mozilla.org/en-US/products/firefox",
    "https://developer.mozilla.org/en-US/docs/Web/JavaScript/Guide",
    "https://www.postgresql.org/docs/current/indexes-partial.html",
    "https://nginx.org/en/docs/beginners_guide.html",
    "https://docs.docker.com/get-started/",
    "https://sqlite.org/windowfunctions.html",
    "https://realpython.com/async-io-python/",
    "https://stackexchange.com/",
    "https://serverfault.com/questions",
    "https://community.spotify.com/",
    "https://www.sitepoint.com/community/",
    "https://forums.tomshardware.com/",
    "https://www.quora.com/",
    "https://www.reddit.com/r/MechanicalKeyboards/",
    "https://www.reddit.com/r/buildapc/",
    "https://www.reddit.com/r/espresso/",
    "https://www.reddit.com/r/DataHoarder/",
    "https://www.ifixit.com/Teardown/iPhone+14+Pro+Teardown/149341",
    "https://www.ifixit.com/Guide/MacBook+Air+13-Inch+Late+2020+Battery+Replacement/139836",
    "https://www.ifixit.com/Teardown/Steam+Deck+Teardown/148869",
    "https://www.tomshardware.com/pc-components/gpus",
    "https://www.gsmarena.com/",
    "https://www.dxomark.com/smartphones/",
    "https://www.wta.org/",
]


async def fetch(url, sem):
    async with sem:
        try:
            async with cr.AsyncSession(impersonate="chrome") as s:
                r = await s.get(url, timeout=25, allow_redirects=True)
                return url, r.status_code, len(r.content)
        except Exception as e:
            return url, "ERR", repr(e)[:40]


async def main():
    sem = asyncio.Semaphore(8)
    res = await asyncio.gather(*[fetch(u, sem) for u in URLS])
    ok = 0
    for u, st, by in res:
        flag = ""
        if st == 200:
            ok += 1
        print(f"{u[:70]:70s} {st!s:>4} {by:>8} {flag}")
    print(f"\n200s: {ok}/36")


asyncio.run(main())
