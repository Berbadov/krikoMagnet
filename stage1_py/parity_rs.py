"""Stage 1B: verify Rust ranker numerical parity with Python (full-rank identity)
and measure Rust p50/p95 latency."""
import json
import os
import subprocess
import sys
import time

from ranker import Ranker

ROOT = os.path.dirname(os.path.abspath(__file__))
CORPUS = os.path.join(ROOT, "..", "data", "corpus.jsonl")
QUERIES = os.path.join(ROOT, "..", "data", "queries.json")
TRANCO = os.path.join(ROOT, "..", "data", "tranco_top100k.txt")
RUST_BIN = os.path.join(ROOT, "..", "stage1_rs", "target", "release", "kriko-magnet-ranker.exe")


def run_python(now_epoch):
    r = Ranker(now=now_epoch)
    r.load_corpus()
    queries = r.load_queries()
    out = {}
    for q in queries:
        ranked = r.rank(q["text"])
        out[q["id"]] = [d["id"] for d in ranked]
    return out


def run_rust(now_epoch):
    cp = subprocess.run(
        [RUST_BIN, "--corpus", CORPUS, "--queries", QUERIES,
         "--tranco", TRANCO, "--now", str(now_epoch)],
        capture_output=True, text=True, check=True,
    )
    return json.loads(cp.stdout)


def main():
    now_epoch = time.time()
    py = run_python(now_epoch)
    rs = run_rust(now_epoch)

    assert set(py.keys()) == set(rs.keys()), "query id mismatch"
    total = 0
    mism = []
    for qid in py:
        a, b = py[qid], rs[qid]
        total += 1
        if a != b:
            # locate first divergence
            first = next((i for i in range(min(len(a), len(b))) if a[i] != b[i]), "len")
            mism.append((qid, first, len(a), len(b)))
            # top-5 check separately
        # top-5 check
    top5_mism = []
    for qid in py:
        if py[qid][:5] != rs[qid][:5]:
            top5_mism.append(qid)

    if mism or top5_mism:
        # show details
        for qid, first, la, lb in mism:
            print(f"  FULL-RANK MISMATCH {qid}: first divergence at idx {first} (py_len={la}, rs_len={lb})")
        for qid in top5_mism:
            print(f"  TOP-5 MISMATCH {qid}: py={py[qid][:5]} vs rs={rs[qid][:5]}")
        print(f"PARITY: FAIL ({len(mism)}/{total} full-rank mismatches; {len(top5_mism)}/15 top-5 mismatches)")
        sys.exit(1)
    else:
        print(f"PARITY: PASS — full-rank identity for all {total} queries; top-5 identical (rank correlation = 1.0)")

    # Rust latency benchmark
    print("\n--- Rust latency benchmark ---")
    bench = subprocess.run(
        [RUST_BIN, "--corpus", CORPUS, "--queries", QUERIES,
         "--tranco", TRANCO, "--now", str(now_epoch), "--bench"],
        capture_output=True, text=True, check=True,
    )
    print(bench.stdout)


if __name__ == "__main__":
    main()
