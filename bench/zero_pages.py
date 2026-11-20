import json

d = json.load(open("bench/out/quality.json", encoding="utf-8"))
mag = {json.loads(l)["url"]: json.loads(l) for l in open("bench/out/magnet.jsonl", encoding="utf-8")}
for r in d["rows"]:
    if r["mag"]["chars"] < 300:
        m = mag[r["url"]]
        print(r["mag"]["chars"], m.get("cached_markdown_chars"), r["url"])
