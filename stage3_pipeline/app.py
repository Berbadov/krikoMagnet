"""Minimal FastAPI endpoint: POST /search {query, k} -> structured JSON."""
import asyncio
import os
import threading

from fastapi import FastAPI
from pydantic import BaseModel

from pipeline import Searcher

app = FastAPI(title="Kriko Magnet", version="0.2.0")
searcher = Searcher(page_budget_ms=int(os.environ.get("KRIKO_PAGE_BUDGET_MS", "0")))


class SearchRequest(BaseModel):
    query: str
    k: int = 3


@app.get("/health")
def health():
    from sites.discovery import BREAKER
    return {
        "status": "ok",
        "index": searcher.index.stats().as_dict() if searcher.index else None,
        "circuit": BREAKER.snapshot(),
    }


@app.post("/search")
async def search(req: SearchRequest):
    return await searcher.search(req.query, k=req.k)


def _refresh_loop(minutes: float) -> None:
    from sites.index import build

    while True:
        threading.Event().wait(minutes * 60)
        try:
            asyncio.run(build(full=False, recent=12))
        except Exception:
            pass


def _start_refresh() -> None:
    minutes = float(os.environ.get("KRIKO_REFRESH_MINUTES", "0") or 0)
    if minutes > 0:
        threading.Thread(target=_refresh_loop, args=(minutes,), daemon=True).start()


_start_refresh()


if __name__ == "__main__":
    import uvicorn
    uvicorn.run(app, host="127.0.0.1", port=8000)
