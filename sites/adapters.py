"""Per-site discovery adapters: own search page, sitemap/RSS, URL patterns."""
from __future__ import annotations

import asyncio
import json
import math
import re
import time
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field
from html import unescape
from urllib.parse import quote_plus, urljoin, urlparse

from selectolax.lexbor import LexborHTMLParser as HTMLParser

from .text import fold, iso_date, match_tokens, query_terms, slugify, stem_token, unslug

ASSET_EXT = re.compile(r"\.(?:jpe?g|png|gif|webp|svg|ico|css|js|woff2?|ttf|zip|gz|mp4|mp3|pdf)(?:$|\?)", re.I)
MODEL_TOKEN = re.compile(r"^(?=.*[a-z])(?=.*\d)[a-z0-9]{2,8}$")
SITEMAP_MEMORY_TTL = 3600
_SITEMAP_MEMORY: dict[str, tuple[float, str, list[dict]]] = {}


def clear_sitemap_memory():
    _SITEMAP_MEMORY.clear()


async def load_xml(fetcher, url: str):
    """Fetch and parse a sitemap/feed once per process (bounded TTL)."""
    now = time.monotonic()
    hit = _SITEMAP_MEMORY.get(url)
    if hit and now - hit[0] < SITEMAP_MEMORY_TTL:
        return hit[1], hit[2]
    res = await fetcher.get(url)
    if not res.ok:
        return "", []
    kind, entries = parse_xml(res.text)
    if kind:
        _SITEMAP_MEMORY[url] = (now, kind, entries)
    return kind, entries
GENERIC_QUERY_TERMS = {
    "recall", "recalls", "review", "reviews", "test", "tests", "best", "problem",
    "problems", "issue", "issues", "fix", "guide", "price", "buy", "vs",
    "comparison", "error", "errors", "code", "codes", "fault", "faults",
    "ariza", "sorun", "service", "servis", "warranty", "garanti", "replace",
    "replacement", "repair", "teardown", "tamir", "degisim", "yorum", "inceleme",
    "update", "updates",
}
PRODUCT_TERMS = {
    "dishwasher", "washing", "washer", "dryer", "refrigerator", "fridge", "oven",
    "vacuum", "boiler", "kombi", "tv", "monitor", "laptop", "phone", "smartphone",
    "iphone", "ipad", "macbook", "airpods", "treadmill",
    "telefon", "buzdolabi", "camasir", "bulasik", "firin", "supurge", "araba",
    "arac", "machine", "makine", "oyuncak",
}
BRAND_TERMS = {
    "apple", "iphone", "ipad", "macbook", "airpods", "samsung", "lg", "sony",
    "anker", "dyson", "whirlpool", "bosch", "vaillant", "honda", "toyota",
    "ford", "tesla", "renault", "xiaomi", "redmi", "google", "pixel", "oneplus",
    "oppo", "vivo", "hyundai", "kia", "bmw", "mercedes", "audi", "volkswagen",
    "skoda", "dacia", "peugeot", "citroen", "opel", "fiat", "arcelik", "beko",
    "philips", "panasonic", "tcl", "hisense", "miele", "siemens", "zanussi",
    "insignia", "maytag", "kitchenaid", "ryzen", "nvidia", "amd", "intel",
    "sonos", "peloton", "steam", "baymak", "demirdokum", "eca", "buderus",
    "vestel", "altus", "regal", "tefal", "arzum", "fakir", "karaca",
}


@dataclass
class Candidate:
    url: str
    title: str
    snippet: str = ""
    published: str | None = None
    source: str = ""
    method: str = ""
    score: float = 0.0


@dataclass
class AdapterOutcome:
    site: str
    method: str
    status: str
    candidates: list[Candidate] = field(default_factory=list)
    fetched: int = 0
    elapsed_ms: float = 0.0
    error: str | None = None


def _local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1].lower()


def parse_xml(text: str) -> tuple[str, list[dict]]:
    """Return (root kind, entries) for sitemap, sitemapindex or RSS."""
    text = text.lstrip("\ufeff \r\n\t")
    try:
        root = ET.fromstring(text)
    except ET.ParseError:
        return "", []
    kind = _local(root.tag)
    entries: list[dict] = []
    if kind == "sitemapindex":
        for node in root:
            if _local(node.tag) != "sitemap":
                continue
            item = {}
            for child in node:
                key = _local(child.tag)
                if key in ("loc", "lastmod"):
                    item[key] = (child.text or "").strip()
            if item.get("loc"):
                entries.append(item)
        return "sitemapindex", entries
    if kind == "urlset":
        for node in root:
            if _local(node.tag) != "url":
                continue
            item = {}
            for child in node:
                key = _local(child.tag)
                if key in ("loc", "lastmod", "title"):
                    item[key] = (child.text or "").strip()
            if item.get("loc"):
                item["_text"] = fold(unslug(item["loc"]))
                entries.append(item)
        return "urlset", entries
    if kind == "rss":
        channel = next((n for n in root if _local(n.tag) == "channel"), None)
        if channel is None:
            return "rss", []
        for node in channel:
            if _local(node.tag) != "item":
                continue
            item = {}
            for child in node:
                key = _local(child.tag)
                if key in ("title", "link", "description", "pubdate", "category"):
                    item["pubDate" if key == "pubdate" else key] = (child.text or "").strip()
            if item.get("link"):
                item["_text"] = fold(f"{item.get('title', '')} {item.get('description', '')} {item.get('category', '')}")
                entries.append(item)
        return "rss", entries
    return kind, entries


def _token_match(term: str, tokens: set[str]) -> bool:
    return term in tokens


def score_candidates(query: str, candidates: list[Candidate], max_results: int = 40) -> list[Candidate]:
    """IDF-weighted lexical match over title+snippet."""
    from .text import product_aliases

    terms = [t for t in dict.fromkeys(stem_token(t) for t in query_terms(query))]
    for term in list(terms):
        for synonym in (("replacement", "replace") if term == "upgrade" else ()):
            if synonym not in terms:
                terms.append(synonym)
        for alias in product_aliases(term):
            if alias not in terms:
                terms.append(alias)
    if not terms or not candidates:
        return []
    alpha_terms = [t for t in terms if any(ch.isalpha() for ch in t)]
    specific_terms = [t for t in terms if t not in GENERIC_QUERY_TERMS]
    texts = [match_tokens(f"{c.title} {c.snippet}") for c in candidates]
    n = len(candidates)
    weights = {}
    for term in terms:
        df = sum(1 for toks in texts if _token_match(term, toks))
        weight = math.log(1 + n / (1 + df))
        if df == 0:
            weight = 0.0
        if term.isdigit():
            weight *= 0.7
        elif MODEL_TOKEN.match(term):
            weight *= 2.5
        weights[term] = weight
    total = sum(weights.values()) or 1.0
    qfold = fold(query)
    qterms = query_terms(query)
    qcompounds = {a + b for a, b in zip(qterms, qterms[1:]) if (a + b) in qterms}
    qwords = [t for t in qterms
              if t not in GENERIC_QUERY_TERMS and t not in qcompounds]
    query_phrase = " ".join(qwords) if len(qwords) >= 2 else ""
    numeric_terms = [t for t in terms if t.isdigit()]
    scored = []
    min_specific = 2 if len(specific_terms) >= 2 else 1
    for cand, toks in zip(candidates, texts):
        matched = [t for t in terms if weights[t] > 0 and _token_match(t, toks)]
        if not matched:
            continue
        if alpha_terms and not any(t in matched for t in alpha_terms):
            continue
        specific_matched = [t for t in matched if t in specific_terms]
        if specific_terms and not specific_matched:
            continue
        score = sum(weights[t] for t in matched) / total
        if score <= 0:
            continue
        raw = fold(f"{cand.url} {cand.title} {cand.snippet}")
        if query_phrase:
            flat = re.sub(r"[-_/]+", " ", raw)
            if query_phrase in flat:
                score += 0.30  # exact model/name phrase, e.g. "rtx 4070 super"
        if ("review" in qfold or "inceleme" in qfold) and ("review" in raw or "inceleme" in raw):
            score += 0.25
        if ("fiyat" in qfold or "price" in qfold) and ("fiyat" in raw or "price" in raw):
            score += 0.25
        if ("recall" in qfold or "recall" in raw) and "recall" in raw and "recall" in qfold:
            score += 0.10
        if any(t in qfold for t in ("repair", "replacement", "replace", "tamir", "degisim")):
            if any(t in raw for t in ("repair", "replacement", "replace", "tamir", "degisim", "guide")):
                score += 0.15
        if numeric_terms and any(t in toks for t in numeric_terms):
            score += 0.10
        cand.score = round(score, 4)
        scored.append((cand, cand.score, len(specific_matched)))

    strict = [x for x in scored if x[2] >= min_specific]
    if strict:
        chosen = strict
    elif any(MODEL_TOKEN.match(t) and weights[t] == 0 for t in specific_terms):
        chosen = scored  # a model code was absent; allow one-term evidence
    else:
        chosen = []
    chosen.sort(key=lambda x: (x[1], x[2], x[0].published or ""), reverse=True)
    return [cand for cand, _, _ in chosen[:max_results]]


def enforce_model_terms(query: str, candidates: list[Candidate]) -> list[Candidate]:
    """If a model code exists, prefer pages matching it plus a context term."""
    model_terms = [t for t in query_terms(query) if MODEL_TOKEN.match(t)]
    if not model_terms:
        return candidates
    context_terms = [
        t for t in query_terms(query)
        if t not in model_terms and t not in GENERIC_QUERY_TERMS
    ]

    def tokens_of(cand):
        return match_tokens(f"{cand.url} {cand.title} {cand.snippet}")

    def has_model(toks):
        return any(_token_match(term, toks) for term in model_terms)

    if context_terms:
        both = []
        for cand in candidates:
            toks = tokens_of(cand)
            if has_model(toks) and any(_token_match(t, toks) for t in context_terms):
                both.append(cand)
        if both:
            return both

    model_only = [cand for cand in candidates if has_model(tokens_of(cand))]
    return model_only or candidates


def enforce_brand_terms(query: str, candidates: list[Candidate]) -> list[Candidate]:
    """When the query names a brand, keep pages that mention it."""
    brands = [t for t in query_terms(query) if t in BRAND_TERMS]
    if not brands:
        return candidates
    brand_tokens = {stem_token(b) for b in brands}
    matched = []
    for cand in candidates:
        toks = match_tokens(f"{cand.url} {cand.title} {cand.snippet}")
        if any(brand in toks for brand in brand_tokens):
            matched.append(cand)
    return matched


def enforce_product_terms(query: str, candidates: list[Candidate]) -> list[Candidate]:
    """When the query names a product type, keep pages about that product.

    Turkish and English product nouns are treated as equivalents, so
    "samsung buzdolabı sorunu" keeps the English "Samsung refrigerator" hub.
    """
    from .text import product_aliases

    products = [t for t in query_terms(query) if t in PRODUCT_TERMS]
    if not products:
        return candidates
    product_tokens = set()
    for product in products:
        product_tokens |= product_aliases(product)
    matched = []
    for cand in candidates:
        toks = match_tokens(f"{cand.url} {cand.title} {cand.snippet}")
        if any(product in toks for product in product_tokens):
            matched.append(cand)
    return matched


def enforce_intent_terms(query: str, candidates: list[Candidate]) -> list[Candidate]:
    """Price/review intent should beat same-product repair pages."""
    q = fold(query)
    if "fiyat" in q or "price" in q:
        markers = ("fiyat", "price")
    elif "review" in q or "inceleme" in q:
        markers = ("review", "inceleme", "faceoff", "handson", "hands-on")
    else:
        return candidates
    matched = []
    for cand in candidates:
        text = fold(f"{cand.url} {cand.title}")
        if any(marker in text for marker in markers):
            matched.append(cand)
    return matched or candidates


def _select_children(entries: list[dict], method: dict, max_sitemaps: int) -> list[dict]:
    include = [re.compile(p) for p in method.get("include_children", [])]
    exclude = [re.compile(p) for p in method.get("exclude_children", [])]
    selected = []
    for item in entries:
        loc = item["loc"]
        if include and not any(p.search(loc) for p in include):
            continue
        if exclude and any(p.search(loc) for p in exclude):
            continue
        selected.append(item)

    def key(item):
        loc = item.get("loc", "")
        found = re.search(r"(19\d{2}|20\d{2})[-_](\d{1,2})(?=[^0-9]|$)", loc)
        if found:
            return f"{found.group(1)}-{int(found.group(2)):02d}"
        return item.get("lastmod") or ""

    selected.sort(key=key, reverse=True)
    return selected[:max_sitemaps]


async def run_sitemap(site: dict, method: dict, query: str, fetcher) -> AdapterOutcome:
    t0 = time.perf_counter()
    max_sitemaps = int(method.get("max_sitemaps", 4))
    max_urls = int(method.get("max_urls", 20000))
    max_results = int(method.get("max_results", 40))
    index_urls = list(method.get("urls") or ([method["url"]] if method.get("url") else []))
    entries: list[dict] = []
    fetched = 0
    statuses = []

    for url in index_urls:
        res = await fetcher.get(url)
        fetched += 1
        if res.robots_skipped:
            statuses.append("robots-denied")
            continue
        if not res.ok:
            statuses.append(str(res.status))
            continue
        kind, items = await load_xml(fetcher, url)
        statuses.append(kind or "unparsed")
        if kind == "sitemapindex":
            children = _select_children(items, method, max_sitemaps)
            child_results = await asyncio.gather(*[load_xml(fetcher, c["loc"]) for c in children])
            for child_kind, child_items in child_results:
                fetched += 1
                if child_kind in ("urlset", "rss"):
                    entries.extend(child_items)
        elif kind in ("urlset", "rss"):
            entries.extend(items)

    if statuses and all(s == "robots-denied" for s in statuses):
        return AdapterOutcome(site["id"], "sitemap", "robots-denied", [], fetched,
                              (time.perf_counter() - t0) * 1000)

    domain = site["domain"].lower()
    terms = [stem_token(t) for t in query_terms(query)]
    need = 2 if len(terms) >= 3 else 1
    # Cheap substring prefilter first; if no URL has two matches, relax to one.
    if need == 2:
        any_two = False
        for item in entries:
            text = item.get("_text") or fold(unslug(item.get("loc") or ""))
            hits = sum(1 for t in terms if t in text)
            if hits >= 2:
                any_two = True
                break
        if not any_two:
            need = 1

    candidates = []
    seen = set()
    for item in entries:
        url = item.get("loc") or item.get("link") or ""
        if not url or url in seen:
            continue
        text = item.get("_text") or fold(unslug(url))
        if sum(1 for t in terms if t in text) < need:
            continue
        seen.add(url)
        if ASSET_EXT.search(url):
            continue
        host = urlparse(url).netloc.lower()
        if domain not in host:
            continue
        title = item.get("title") or unslug(url)
        snippet = f"{site.get('name', '')} {unslug(url)}"
        candidates.append(Candidate(
            url=url,
            title=title[:300],
            snippet=snippet[:500],
            published=iso_date(item.get("lastmod") or item.get("pubDate")),
            source=site["id"],
            method="sitemap",
        ))
        if len(candidates) >= max_urls:
            break

    ranked = score_candidates(query, candidates, max_results=max_results)
    return AdapterOutcome(site["id"], "sitemap", "ok" if ranked else "no-results",
                          ranked, fetched, (time.perf_counter() - t0) * 1000)


async def run_search(site: dict, method: dict, query: str, fetcher) -> AdapterOutcome:
    t0 = time.perf_counter()
    url = (method.get("url") or "").replace("{query}", quote_plus(query))
    res = await fetcher.get(url)
    if res.robots_skipped:
        return AdapterOutcome(site["id"], "search", "robots-denied", [], 1,
                              (time.perf_counter() - t0) * 1000)
    if not res.ok:
        return AdapterOutcome(site["id"], "search", "error", [], 1,
                              (time.perf_counter() - t0) * 1000, f"HTTP {res.status}")
    selector = method.get("link_selector", "a[href]")
    base = method.get("base_url") or url
    max_results = int(method.get("max_results", 20))
    domain = site["domain"].lower()
    candidates = []
    seen = set()
    try:
        tree = HTMLParser(res.text)
        anchors = tree.css(selector)
    except Exception as exc:
        return AdapterOutcome(site["id"], "search", "error", [], 1,
                              (time.perf_counter() - t0) * 1000, repr(exc)[:120])
    for a in anchors:
        href = (a.attributes.get("href") or "").strip()
        if not href or href.startswith("#") or href.startswith("javascript:"):
            continue
        link = urljoin(base, href)
        if domain not in urlparse(link).netloc.lower():
            continue
        if ASSET_EXT.search(link) or link in seen:
            continue
        seen.add(link)
        title = (a.text(strip=True) or a.attributes.get("title") or unslug(link))[:300]
        parent = a.parent
        snippet = title
        if parent is not None:
            extra = parent.text(separator=" ", strip=True)
            if extra and extra != title:
                snippet = f"{title} {extra[:300]}"
        candidates.append(Candidate(
            url=link, title=title, snippet=snippet[:500],
            source=site["id"], method="search",
        ))
        if len(candidates) >= max_results * 3:
            break
    ranked = score_candidates(query, candidates, max_results=max_results)
    return AdapterOutcome(site["id"], "search", "ok" if ranked else "no-results",
                          ranked, 1, (time.perf_counter() - t0) * 1000)


def _strip_html(value: str) -> str:
    value = re.sub(r"<[^>]+>", " ", value or "")
    return " ".join(unescape(value).split())


async def run_rss(site: dict, method: dict, query: str, fetcher) -> AdapterOutcome:
    t0 = time.perf_counter()
    res = await fetcher.get(method["url"])
    if res.robots_skipped:
        return AdapterOutcome(site["id"], "rss", "robots-denied", [], 1,
                              (time.perf_counter() - t0) * 1000)
    if not res.ok:
        return AdapterOutcome(site["id"], "rss", "error", [], 1,
                              (time.perf_counter() - t0) * 1000, f"HTTP {res.status}")
    kind, items = await load_xml(fetcher, method["url"])
    if kind != "rss":
        return AdapterOutcome(site["id"], "rss", "error", [], 1,
                              (time.perf_counter() - t0) * 1000, "not an RSS feed")
    max_results = int(method.get("max_results", 20))
    candidates = []
    for item in items:
        title = _strip_html(item.get("title", ""))
        desc = _strip_html(item.get("description", ""))
        category = _strip_html(item.get("category", ""))
        display = f"{category}: {title}" if category else title
        candidates.append(Candidate(
            url=item["link"], title=display[:300],
            snippet=desc[:500],
            published=iso_date(item.get("pubDate")),
            source=site["id"], method="rss",
        ))
    ranked = score_candidates(query, candidates, max_results=max_results)
    return AdapterOutcome(site["id"], "rss", "ok" if ranked else "no-results",
                          ranked, 1, (time.perf_counter() - t0) * 1000)


API_GENERIC = {
    "recall", "recalls", "recalled", "review", "reviews", "test", "best", "vs",
    "problem", "problems", "issue", "issues", "fix", "guide", "price", "buy",
}


async def run_api(site: dict, method: dict, query: str, fetcher) -> AdapterOutcome:
    t0 = time.perf_counter()
    template = method["url"]
    product_terms = [t for t in query_terms(query) if t not in API_GENERIC]
    if "treadmill" in product_terms:
        product_terms.append("tread")
    probes = [query] + product_terms[:3]
    probes = list(dict.fromkeys(p for p in probes if p))[: int(method.get("probes", 4))]
    max_results = int(method.get("max_results", 20))
    candidates = []
    seen = set()
    fetched = 0
    for probe in probes:
        url = template.replace("{query}", quote_plus(probe))
        res = await fetcher.get(url)
        fetched += 1
        if res.robots_skipped or not res.ok:
            continue
        try:
            data = json.loads(res.text)
        except Exception:
            continue
        if not isinstance(data, list):
            continue
        for item in data:
            link = (item.get("URL") or "").strip()
            if not link or link in seen:
                continue
            seen.add(link)
            title = (item.get("Title") or "").strip() or unslug(link)
            desc = (item.get("Description") or "").strip()
            candidates.append(Candidate(
                url=link, title=title[:300], snippet=desc[:600],
                published=iso_date(item.get("RecallDate") or item.get("LastPublishDate")),
                source=site["id"], method="api",
            ))
    ranked = score_candidates(query, candidates, max_results=max_results)
    return AdapterOutcome(site["id"], "api", "ok" if ranked else "no-results",
                          ranked, fetched, (time.perf_counter() - t0) * 1000)


async def run_url_pattern(site: dict, method: dict, query: str, fetcher) -> AdapterOutcome:
    t0 = time.perf_counter()
    terms = query_terms(query)
    limit = method.get("max_query_terms")
    if limit and len(terms) > int(limit):
        return AdapterOutcome(site["id"], "url_pattern", "no-results", [], 0,
                              (time.perf_counter() - t0) * 1000)
    slug = slugify(query)
    candidates = []
    for template in method.get("templates", []):
        url = template.replace("{slug}", slug).replace("{query}", quote_plus(query))
        candidates.append(Candidate(
            url=url, title=unslug(url), snippet=f"{site.get('name', '')} {slug}",
            source=site["id"], method="url_pattern",
        ))
    ranked = score_candidates(query, candidates, max_results=int(method.get("max_results", 5)))
    return AdapterOutcome(site["id"], "url_pattern", "ok" if ranked else "no-results",
                          ranked, 0, (time.perf_counter() - t0) * 1000)


async def run_site(site: dict, query: str, fetcher) -> list[AdapterOutcome]:
    """Run methods in registry order and stop at the first useful one."""
    outcomes = []
    for method in site["methods"]:
        kind = method["type"]
        if kind == "search":
            outcome = await run_search(site, method, query, fetcher)
        elif kind == "sitemap":
            outcome = await run_sitemap(site, method, query, fetcher)
        elif kind == "rss":
            outcome = await run_rss(site, method, query, fetcher)
        elif kind == "api":
            outcome = await run_api(site, method, query, fetcher)
        elif kind == "url_pattern":
            outcome = await run_url_pattern(site, method, query, fetcher)
        else:
            continue
        outcomes.append(outcome)
        if outcome.candidates:
            break
        if outcome.status == "robots-denied":
            continue
    return outcomes
