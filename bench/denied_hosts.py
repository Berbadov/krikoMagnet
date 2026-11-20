import json
from collections import Counter
from urllib.parse import urlsplit

rows = [json.loads(l) for l in open("bench/out/e2e.jsonl", encoding="utf-8")]
c = Counter(urlsplit(u).netloc for r in rows if r["phase"] == "cold" for u in r["denied"])
print(c.most_common(12))
urls = Counter(urlsplit(u).netloc for r in rows if r["phase"] == "cold" for u in r["search_urls"])
print(urls.most_common(12))
