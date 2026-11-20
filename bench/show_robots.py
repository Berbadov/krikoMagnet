import sqlite3, sys

c = sqlite3.connect("file:data/bench_fetch.sqlite?mode=ro", uri=True)
for o, s, b, t in c.execute("SELECT origin,status,body,fetched_at FROM robots"):
    if any(k in o for k in ("donanim", "shiftdelete", "rtings")):
        print(o, s, len(b or ""))
        print((b or "")[:800])
        print("-" * 40)
