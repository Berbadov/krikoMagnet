import sqlite3, sys, os

for p in sys.argv[1:]:
    print("==", p, os.path.exists(p))
    if not os.path.exists(p):
        continue
    c = sqlite3.connect("file:" + p.replace("\\", "/") + "?mode=ro", uri=True)
    tables = [r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='table'")]
    for t in tables:
        print(t, c.execute("SELECT count(*) FROM " + t).fetchone()[0])
        print("  cols:", [d[1] for d in c.execute("PRAGMA table_info(%s)" % t)])
