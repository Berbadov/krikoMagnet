"""Resident Rust ranker client (JSON-lines) with a one-shot fallback.

The ranker binary is started once per process (``--serve``): Tranco is loaded
once and every query only ships the candidate corpus. If the child dies or
times out, the client restarts it and falls back to the original one-shot
subprocess so a query still completes.
"""
from __future__ import annotations

import json
import os
import queue
import subprocess
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.join(HERE, "..")
DEFAULT_BIN = os.path.join(ROOT, "stage1_rs", "target", "release", "kriko-magnet-ranker.exe")
DEFAULT_TRANCO = os.path.join(ROOT, "data", "tranco_top100k.txt")


class Ranker:
    def __init__(self, bin_path: str = DEFAULT_BIN, tranco: str = DEFAULT_TRANCO,
                 timeout: float = 20.0):
        self.bin = bin_path
        self.tranco = tranco
        self.timeout = timeout
        self._proc: subprocess.Popen | None = None
        self._queue: queue.Queue | None = None
        self._reader: threading.Thread | None = None
        self._lock = threading.Lock()
        self._restarts = 0

    # -- resident process ------------------------------------------------

    def _start(self) -> None:
        self._proc = subprocess.Popen(
            [self.bin, "--serve", "--tranco", self.tranco],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            text=True, encoding="utf-8", bufsize=1,
        )
        q: queue.Queue = queue.Queue()

        def read():
            try:
                for line in self._proc.stdout:
                    q.put(line)
            finally:
                q.put(None)

        self._queue = q
        self._reader = threading.Thread(target=read, daemon=True)
        self._reader.start()

    def _stop(self) -> None:
        proc, self._proc = self._proc, None
        if proc is not None:
            try:
                proc.kill()
            except Exception:
                pass

    def rank(self, records: list[dict], text: str, now: float | None = None,
             scores: bool = True) -> list[dict] | None:
        if not os.path.exists(self.bin):
            return None
        request = json.dumps({
            "corpus": records, "text": text, "now": now or time.time(), "scores": scores,
        }, ensure_ascii=False)
        with self._lock:
            for attempt in range(2):
                try:
                    if self._proc is None or self._proc.poll() is not None:
                        self._start()
                    self._proc.stdin.write(request + "\n")
                    self._proc.stdin.flush()
                    line = self._queue.get(timeout=self.timeout)
                    if line is None:
                        raise RuntimeError("ranker closed stdout")
                    data = json.loads(line)
                    if isinstance(data, dict) and data.get("error"):
                        raise RuntimeError(data["error"])
                    return data
                except Exception:
                    self._stop()
                    self._restarts += 1
                    if attempt:
                        break
        return self._one_shot(records, text, now, scores)

    def _one_shot(self, records, text, now, scores):
        with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False,
                                         encoding="utf-8") as cf:
            for rec in records:
                cf.write(json.dumps(rec, ensure_ascii=False) + "\n")
            corpus_path = cf.name
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False,
                                         encoding="utf-8") as qf:
            json.dump([{"id": "q0", "text": text}], qf, ensure_ascii=False)
            qpath = qf.name
        try:
            cp = subprocess.run(
                [self.bin, "--corpus", corpus_path, "--queries", qpath,
                 "--tranco", self.tranco, "--now", str(now or time.time()),
                 *(["--scores"] if scores else [])],
                capture_output=True, text=True, timeout=self.timeout,
            )
            data = json.loads(cp.stdout)
            return data.get("q0", [])
        except Exception:
            return None
        finally:
            for path in (corpus_path, qpath):
                try:
                    os.unlink(path)
                except OSError:
                    pass

    def close(self) -> None:
        self._stop()

    @property
    def restarts(self) -> int:
        return self._restarts
