"""Minimal FastAPI endpoint: POST /search {query, k} -> structured JSON."""
import os

from fastapi import FastAPI
from pydantic import BaseModel

from pipeline import Searcher

app = FastAPI(title="Kriko Magnet", version="0.1.0")
searcher = Searcher()


class SearchRequest(BaseModel):
    query: str
    k: int = 3


@app.get("/health")
def health():
    return {"status": "ok"}


@app.post("/search")
async def search(req: SearchRequest):
    return await searcher.search(req.query, k=req.k)


if __name__ == "__main__":
    import uvicorn
    uvicorn.run(app, host="127.0.0.1", port=8000)
