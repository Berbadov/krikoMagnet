"""Clean-text extraction shared by Stage 2, Stage 3 and the adapters."""
from __future__ import annotations

SOFT_MIN_CHARS = 300

try:
    from selectolax.lexbor import LexborHTMLParser as HTMLParser
except Exception:  # pragma: no cover - older selectolax
    from selectolax.parser import HTMLParser


TEXT_CHALLENGE_MARKERS = [
    "just a moment", "enable javascript and cookies", "verify you are human",
    "captcha", "attention required", "access denied", "checking your browser",
]


def extract_text(html: str, min_chars: int = SOFT_MIN_CHARS) -> str:
    text = None
    try:
        import trafilatura
        text = trafilatura.extract(html, include_comments=False, include_tables=True, favor_recall=True)
    except Exception:
        text = None
    if text and len(text) >= min_chars:
        return text
    try:
        tree = HTMLParser(html)
        body = tree.body
        text = body.text(separator=" ") if body is not None else (text or "")
        text = " ".join(text.split())
    except Exception:
        text = text or ""
    return text


def classify_fetch(status, clean_chars: int, text: str) -> str:
    """Stage-2 style verdict for a fetched page."""
    low = (text or "").lower()
    if status in (403, 429, 401):
        return "hard-block"
    if status == "robots-denied":
        return "robots-denied"
    if status != 200:
        return "http-error"
    marker = any(m in low for m in TEXT_CHALLENGE_MARKERS)
    if clean_chars < SOFT_MIN_CHARS or marker:
        return "soft-block"
    return "success"
