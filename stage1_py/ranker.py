"""Hybrid ranker: BM25 (title+snippet) + log-scaled Tranco authority + freshness."""
import json
import math
import os
import re
from datetime import datetime, timezone

from tranco_cache import load_top100k

DATA_DIR = os.path.join(os.path.dirname(__file__), "..", "data")
TOKEN_RE = re.compile(r"[a-z0-9]+")

# default weights (additive over lexically-gated candidates)
DEFAULT_W = {"alpha": 0.40, "beta": 0.60, "gamma": 0.15}
PRIOR_MAX = math.log(1_000_000)


def tokenize(text: str) -> list[str]:
    return TOKEN_RE.findall(text.lower())


def iso_to_dt(s: str) -> datetime:
    return datetime.fromisoformat(s.replace("Z", "+00:00"))


class Ranker:
    def __init__(self, weights=None, tau_days: float = 90.0, now=None):
        self.w = {**(DEFAULT_W if weights is None else weights)}
        self.tau = tau_days
        self.k1 = 1.2
        self.b = 0.75
        self.docs = []
        self.avgdl = 0.0
        self.N = 0
        self.df = {}          # term -> doc frequency
        self.domain_rank = {} # base domain -> rank (1-based)
        self.now = None
        if now is not None:  # epoch seconds; injected for exact parity
            self.now = datetime.fromtimestamp(now, tz=timezone.utc)
        self._load_tranco()

    def _ref_now(self):
        return self.now if self.now is not None else datetime.now(timezone.utc)

    def _load_tranco(self):
        for i, d in enumerate(load_top100k()):
            self.domain_rank.setdefault(d, i + 1)

    def _prior(self, domain: str) -> float:
        r = self.domain_rank.get(domain)
        if r is None:
            return 0.0
        return math.log(1_000_000 / max(r, 1))

    def load_corpus(self, path=None):
        path = path or os.path.join(DATA_DIR, "corpus.jsonl")
        with open(path, encoding="utf-8") as f:
            self.docs = [json.loads(line) for line in f]
        self.N = len(self.docs)
        lens = []
        self.df = {}
        for d in self.docs:
            toks = tokenize(d["title"] + " " + d["snippet"])
            lens.append(len(toks))
            for t in set(toks):
                self.df[t] = self.df.get(t, 0) + 1
        self.avgdl = sum(lens) / self.N if self.N else 0.0

    def _bm25_raw(self, qterms, doc):
        toks = tokenize(doc["title"] + " " + doc["snippet"])
        dl = len(toks)
        if dl == 0:
            return 0.0
        tf = {}
        for t in toks:
            tf[t] = tf.get(t, 0) + 1
        score = 0.0
        for t in set(qterms):
            f = tf.get(t, 0)
            if f == 0:
                continue
            df = self.df.get(t, 0)
            idf = math.log(1 + (self.N - df + 0.5) / (df + 0.5))
            denom = f + self.k1 * (1 - self.b + self.b * dl / self.avgdl)
            score += idf * (f * (self.k1 + 1)) / denom
        return score

    def score_doc(self, qterms, doc):
        # BM25 (normalized later per-query), prior, freshness
        bm25 = self._bm25_raw(qterms, doc)
        prior = self._prior(doc["domain"])
        days = (self._ref_now() - iso_to_dt(doc["published"])).total_seconds() / 86400.0
        fresh = math.exp(-max(days, 0.0) / self.tau)
        return bm25, prior, fresh

    def rank(self, query_text, top_k=None, return_scores=False):
        qterms = tokenize(query_text)
        scored = []
        for doc in self.docs:
            bm25, prior, fresh = self.score_doc(qterms, doc)
            scored.append((doc, bm25, prior, fresh))
        # Lexical gate: only docs with >=1 query-token match are candidates.
        eligible = [s for s in scored if s[1] > 0.0]
        max_bm = max((s[1] for s in eligible), default=1.0)
        if max_bm <= 0:
            max_bm = 1.0
        w = self.w
        out = []
        for doc, bm25, prior, fresh in scored:
            if bm25 <= 0.0:
                out.append((doc, 0.0, 0.0, 0.0, fresh))
                continue
            bm_s = bm25 / max_bm
            prior_s = prior / PRIOR_MAX
            s = w["alpha"] * bm_s + w["beta"] * prior_s + w["gamma"] * fresh
            out.append((doc, s, bm_s, prior_s, fresh))
        out.sort(key=lambda x: x[1], reverse=True)
        if top_k is not None:
            out = out[:top_k]
        if return_scores:
            return out
        return [d for d, *_ in out]

    def load_queries(self, path=None):
        path = path or os.path.join(DATA_DIR, "queries.json")
        with open(path, encoding="utf-8") as f:
            return json.load(f)
