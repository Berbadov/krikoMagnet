import json

d = json.load(open("bench/out/quality.json", encoding="utf-8"))
rows = d["rows"]
print("rows", len(rows))
print("fc <300", sum(1 for r in rows if r["fc"]["chars"] < 300), [r["url"] for r in rows if r["fc"]["chars"] < 300])
print("mag <300", sum(1 for r in rows if r["mag"]["chars"] < 300))
print("mag ==0", sum(1 for r in rows if r["mag"]["chars"] == 0))
print("ref structural", sum(1 for r in rows if r["ref_src"] == "structural"))
m = json.load(open("bench/out/magnet.jsonl", encoding="utf-8")) if False else [json.loads(l) for l in open("bench/out/magnet.jsonl", encoding="utf-8")]
print("html present", sum(1 for r in m if r["in_fetch_cache"]))
