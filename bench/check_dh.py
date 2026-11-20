import json, sqlite3, datetime

rows = [json.loads(l) for l in open("bench/out/extract.jsonl", encoding="utf-8")]
dh = [r for r in rows if "donanimhaber" in r["url"]]
print("dh rows", len(dh), [(r["robots_denied"], r["firecrawl"].get("ok")) for r in dh])
c = sqlite3.connect("file:data/bench_fetch.sqlite?mode=ro", uri=True)
for o, s, t in c.execute("SELECT origin,status,fetched_at FROM robots WHERE origin LIKE '%donanimhaber.com%'"):
    print(o, s, datetime.datetime.fromtimestamp(t))
