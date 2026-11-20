"""Shared polite fetching and extraction utilities."""
from .http import (
    DEFAULT_CACHE,
    PAGE_TTL,
    ROBOTS_TTL,
    USER_AGENT,
    CurlTransport,
    Fetcher,
    FetchResult,
    RateLimiter,
    ResponseCache,
    RobotsCache,
    RobotsPolicy,
    origin_of,
    sha16,
)
from .extract import SOFT_MIN_CHARS, classify_fetch, extract_text

__all__ = [
    "DEFAULT_CACHE", "PAGE_TTL", "ROBOTS_TTL", "USER_AGENT",
    "CurlTransport", "Fetcher", "FetchResult", "RateLimiter",
    "ResponseCache", "RobotsCache", "RobotsPolicy",
    "origin_of", "sha16", "SOFT_MIN_CHARS", "classify_fetch", "extract_text",
]
