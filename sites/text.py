"""Small text helpers shared by the site adapters and the query planner."""
from __future__ import annotations

import re
import unicodedata
from datetime import datetime, timezone

TOKEN_RE = re.compile(r"[a-z0-9]+")

STOPWORDS = {
    "a", "an", "and", "are", "as", "at", "be", "best", "by", "can", "do",
    "does", "for", "from", "how", "i", "in", "is", "it", "my", "of", "on",
    "or", "review", "reviews", "the", "to", "vs", "what", "when", "where",
    "which", "with", "test", "price", "fiyat", "nasil", "nedir", "mi", "mu",
    "bir", "ve", "ile", "icin", "en", "iyi", "hangi", "ne", "mi", "mu",
    "2023", "2024", "2025", "2026", "not", "after", "before",
}

TR_MAP = str.maketrans({
    "ç": "c", "Ç": "c", "ğ": "g", "Ğ": "g", "ı": "i", "İ": "i",
    "ö": "o", "Ö": "o", "ş": "s", "Ş": "s", "ü": "u", "Ü": "u",
    "â": "a", "î": "i", "û": "u",
})


def fold(text: str) -> str:
    """Lowercase, strip diacritics and Turkish-specific letters."""
    text = (text or "").translate(TR_MAP)
    text = unicodedata.normalize("NFKD", text)
    text = "".join(ch for ch in text if not unicodedata.combining(ch))
    return text.lower()


def tokenize(text: str) -> list[str]:
    return TOKEN_RE.findall(fold(text))


def query_terms(query: str, keep_stopwords: bool = False) -> list[str]:
    raw = tokenize(query)
    seen = set()
    out = []
    for token in raw:
        if len(token) < 2 and not token.isdigit():
            continue
        if not keep_stopwords and token in STOPWORDS:
            continue
        if token not in seen:
            seen.add(token)
            out.append(token)
    # "cr-v" tokenizes to "cr" + "v"; keep a compact form for URL matching.
    for a, b in zip(raw, raw[1:]):
        combined = a + b
        if 3 <= len(combined) <= 20 and len(a) <= 3 and len(b) <= 3:
            if not keep_stopwords and (a in STOPWORDS or b in STOPWORDS):
                continue
            if combined not in seen:
                seen.add(combined)
                out.append(combined)
    # "note 7" / "iphone15" style model names are often written as one token.
    for a, b in zip(raw, raw[1:]):
        if (a.isalpha() and b.isdigit()) or (a.isdigit() and b.isalpha()):
            combined = a + b
            if 3 <= len(combined) <= 12 and combined not in seen:
                seen.add(combined)
                out.append(combined)
    return out


def slugify(text: str) -> str:
    return "-".join(query_terms(text, keep_stopwords=True))


def stem_token(token: str) -> str:
    """Very small English/Turkish suffix stripper for lexical matching."""
    if len(token) <= 3 or token.isdigit():
        return token
    if token.endswith("leri") or token.endswith("ları"):
        return token[:-4]
    if token.endswith("ment") and len(token) > 6:
        return token[:-4]
    if token.endswith("ies") and len(token) > 4:
        return token[:-3] + "y"
    if token.endswith("ied") and len(token) > 4:
        return token[:-3] + "y"
    if token.endswith("ing") and len(token) > 5:
        return token[:-3]
    if token.endswith("ed") and len(token) > 4:
        return token[:-2]
    if token.endswith("es") and len(token) > 4:
        return token[:-1]
    if token.endswith("s") and not token.endswith("ss") and len(token) > 3:
        return token[:-1]
    if token.endswith("si") and len(token) > 5:
        return token[:-2]
    if token[-1] in "iıuü" and len(token) > 5:
        return token[:-1]
    return token


def match_tokens(text: str) -> set[str]:
    return {stem_token(t) for t in tokenize(text)}


PRODUCT_GROUPS = [
    {"buzdolabi", "refrigerator", "fridge"},
    {"camasir", "washing", "washer"},
    {"bulasik", "dishwasher"},
    {"firin", "oven"},
    {"supurge", "vacuum"},
    {"kombi", "boiler"},
    {"araba", "arac", "car", "otomobil", "vehicle"},
    {"telefon", "phone", "smartphone"},
    {"televizyon", "tv"},
    {"kulaklik", "headphone", "headphones", "earbuds"},
    {"ekran", "screen", "display"},
    {"batarya", "battery"},
    {"klima", "air conditioner"},
    {"termosifon", "water heater"},
]


def _alias_index() -> dict[str, set[str]]:
    index: dict[str, set[str]] = {}
    for group in PRODUCT_GROUPS:
        aliases = {stem_token(alias) for alias in group}
        for member in group:
            index[member] = aliases
            index[stem_token(member)] = aliases
    return index


_PRODUCT_ALIAS_INDEX = _alias_index()


def product_aliases(term: str) -> set[str]:
    """Turkish/English product nouns that mean the same thing for filtering."""
    return _PRODUCT_ALIAS_INDEX.get(term, {stem_token(term)})


def ranker_text(text: str) -> str:
    """Fold + stem text so the Rust tokenizer sees comparable word forms."""
    return " ".join(stem_token(t) for t in tokenize(text))


def unslug(url: str) -> str:
    path = url.split("://", 1)[-1]
    if "/" in path:
        path = path.split("/", 1)[1]
    path = path.split("?", 1)[0].split("#", 1)[0]
    path = re.sub(r"\.(html?|php\d?|aspx?|jsp)$", "", path, flags=re.I)
    return " ".join(re.sub(r"[-_/]+", " ", path).split())


def iso_now() -> str:
    return datetime.now(timezone.utc).isoformat()


def iso_date(value: str | None) -> str | None:
    if not value:
        return None
    value = value.strip()
    try:
        dt = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        try:
            dt = datetime.strptime(value[:10], "%Y-%m-%d")
        except ValueError:
            return None
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt.astimezone(timezone.utc).isoformat()


def long_ago(days: int = 180) -> str:
    from datetime import timedelta
    return (datetime.now(timezone.utc) - timedelta(days=days)).isoformat()
