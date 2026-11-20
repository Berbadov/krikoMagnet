import json

d = json.load(open("bench/out/quality.json", encoding="utf-8"))
rows = [r for r in d["rows"] if r["fc_ok"]]
rescue = [r["url"] for r in rows if r["mag"]["chars"] < 300 and r["fc"]["chars"] > 2000]
loss = [r["url"] for r in rows if r["fc"]["chars"] < 1000 and r["mag"]["chars"] > 2000]
print("firecrawl rescued", len(rescue))
for u in rescue:
    print("  ", u)
print("firecrawl lost", len(loss))
for u in loss:
    print("  ", u)
