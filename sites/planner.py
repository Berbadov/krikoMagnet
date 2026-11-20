"""Keyword/topic query planning. No LLM, no search engine."""
from __future__ import annotations

import math
import re

from .text import fold, query_terms

TOPIC_TERMS = {
    "phones": {
        "iphone", "ipad", "samsung", "galaxy", "pixel", "xiaomi", "redmi", "oneplus",
        "oppo", "vivo", "huawei", "honor", "realme", "nothing", "phone", "smartphone",
        "telefon", "batarya", "battery", "sarj", "charging", "camera", "kamera",
        "ekran", "screen", "airpods", "anker", "powerbank", "powerbank", "compatibility",
        "compatible", "uyumlu", "uyum", "lg",
    },
    "computing": {
        "laptop", "notebook", "gpu", "rtx", "gtx", "radeon", "cpu", "ryzen", "intel",
        "ssd", "nvme", "ram", "monitor", "gaming", "oyun", "bilgisayar", "islemci",
        "ekran karti", "macbook", "windows", "linux", "compatibility", "compatible",
        "uyumlu", "uyum", "anker", "powerbank", "sarj", "usb", "thunderbolt",
    },
    "tv-audio": {
        "tv", "televizyon", "oled", "qled", "soundbar", "speaker", "hoparlor",
        "kulaklik", "headphone", "headphones", "earbuds", "monitor", "lg", "sony",
        "panasonic", "philips", "tcl", "hisense", "c1", "c2", "g1", "g2", "a95",
        "qn90", "x90", "noise", "burn", "panel",
    },
    "appliances": {
        "dishwasher", "washing", "washer", "dryer", "fridge", "refrigerator", "oven",
        "vacuum", "robot", "bulasik", "camasir", "kurutma", "buzdolabi", "firin",
        "supurge", "ankastre", "ocak", "dyson", "bosch", "arcelik", "beko", "samsung",
        "lg", "whirlpool", "maytag", "kitchenaid", "error", "bulasik makinesi",
        "camasir makinesi", "kurutma makinesi", "derin dondurucu", "davlumbaz",
        "su isitici", "sogutmuyor", "isinmiyor", "su akitiyor", "calismiyor",
        "bozuk", "servis", "yedek parca",
    },
    "cars": {
        "car", "otomobil", "araba", "arac", "renault", "megane", "clio", "captur",
        "ford", "toyota", "honda", "hyundai", "kia", "bmw", "mercedes", "volkswagen",
        "vw", "audi", "skoda", "dacia", "peugeot", "citroen", "opel", "fiat", "dizel",
        "diesel", "motor", "yag", "filtre", "lastik", "brake", "fren", "suv", "sedan",
        "cr-v", "corolla", "golf", "passat", "focus", "clio", "megane", "recall", "crv",
        "ariza lambasi", "servis bakim", "yedek parca", "vites", "motor yagi",
        "yakit", "kasko",
    },
    "boilers": {
        "boiler", "kombi", "vaillant", "ecotec", "buderus", "heating", "isitma",
        "radyator", "petek", "ariza", "fault", "error", "pressure", "basinc",
        "termostat", "thermostat", "radiator", "kombi ariza", "ariza kodu",
        "isinmiyor", "su isitmiyor", "baymak", "demirdokum", "eca",
    },
    "recalls": {
        "recall", "recalled", "recalls", "geri", "cagirma", "toplatma", "guvensiz",
        "unsafe", "safety", "cpsc", "rapex", "gubis", "warning", "hazard",
    },
    "repair": {
        "repair", "replacement", "replace", "fix", "teardown", "tamir", "degisim",
        "degistirme", "parca", "spare", "battery replacement", "screen replacement",
        "kirik", "bozuk", "servis",
    },
    "complaints": {
        "complaint", "complaints", "problem", "issue", "issues", "sorun", "sikayet",
        "ariza", "arizali", "faulty", "warranty", "garanti", "service", "servis",
        "kullanici", "yorum",
    },
    "reviews": {
        "review", "reviews", "inceleme", "test", "yorum", "comparison",
        "karsilastirma", "best", "rated", "vs", "deneyim", "experience",
    },
    "apple": {
        "iphone", "ipad", "macbook", "imac", "airpods", "apple", "ios", "macos",
        "watch", "apple watch",
    },
}

TR_STOPWORDS = {
    "ve", "bir", "ile", "icin", "mi", "mu", "ne", "nasil", "hangi", "fiyat",
    "sorun", "sikayet", "ariza", "servis", "garanti", "yorum", "inceleme",
}

TR_CHARS = set("çğıİöşüÇĞÖŞÜ")


def detect_language(query: str) -> str:
    if any(ch in TR_CHARS for ch in query):
        return "tr"
    tokens = set(re.findall(r"[a-z0-9]+", fold(query)))
    if tokens & TR_STOPWORDS:
        return "tr"
    return "en"


def classify(query: str) -> dict:
    text = fold(query)
    tokens = set(re.findall(r"[a-z0-9]+", text))
    hits: dict[str, int] = {}
    for topic, terms in TOPIC_TERMS.items():
        count = 0
        for term in terms:
            if " " in term:
                if term in text:
                    count += 1
            elif term in tokens or (len(term) >= 4 and any(t.startswith(term) for t in tokens)):
                count += 1
        if count:
            hits[topic] = count
    ordered = sorted(hits, key=lambda t: (-hits[t], t))
    return {
        "language": detect_language(query),
        "topics": ordered,
        "topic_hits": hits,
        "terms": query_terms(query),
    }


def _prior_score(domain: str, ranks: dict[str, int]) -> float:
    rank = ranks.get(domain)
    if not rank:
        return 0.0
    return max(0.0, 100.0 * math.log(1_000_000 / rank) / math.log(1_000_000))


def _term_in(term: str, site_terms: set[str]) -> bool:
    if term in site_terms:
        return True
    if len(term) < 4:
        return False
    # Prefix matching only for reasonably long stems: "oyun" must not make
    # "oyuncak" (toy) look like a computing term.
    return any(
        (len(t) >= 5 and t.startswith(term)) or (len(term) >= 5 and term.startswith(t))
        for t in site_terms if len(t) >= 5
    )


def _domain_tokens(domain: str) -> set[str]:
    return {
        part for label in (domain or "").lower().split(".")
        for part in re.split(r"[-_]", label) if len(part) >= 3
    }


def plan_sites(query: str, registry: dict, tranco_ranks: dict[str, int] | None = None,
               max_sites: int = 6) -> dict:
    """Pick candidate sites: query-term matches, brand/domain, topics, language."""
    tranco_ranks = tranco_ranks or {}
    plan = classify(query)
    topics = set(plan["topics"])
    fallback = None
    if not topics:
        topics = {"general"}
        fallback = "general"
        plan["topics"] = ["general"]
    lang = plan["language"]
    terms = plan["terms"]
    scored = []
    recall_query = "recalls" in topics or any(
        term in {"recall", "recalls", "recalled", "guvensiz", "toplatma", "cagirma"}
        for term in terms)
    for site in registry["sites"]:
        if site["id"] in ("cpsc", "guvensizurun") and not recall_query:
            continue
        site_topics = set(site["topics"])
        overlap = site_topics & topics
        site_terms = set()
        for topic in site_topics:
            site_terms |= TOPIC_TERMS.get(topic, set())
        matched = sum(1 for term in terms if _term_in(term, site_terms))
        if not overlap and not matched:
            continue
        relevance = len(overlap)
        language_bonus = 1 if site.get("language") == lang else 0
        domain_tokens = _domain_tokens(site["domain"])
        domain_hits = sum(1 for term in terms if len(term) >= 3 and term in domain_tokens)
        prior = _prior_score(site["domain"], tranco_ranks)
        score = (matched * 1000 + relevance * 100 + language_bonus * 300
                 + domain_hits * 800 + float(site.get("priority", 0)) + prior)
        scored.append((score, site, matched))
    if not scored:
        # Nothing matched: prefer general reference sites, then broad reviews.
        for topic in ("general", "reviews"):
            for site in registry["sites"]:
                if topic in site["topics"]:
                    scored.append((_prior_score(site["domain"], tranco_ranks), site, 0))
            if scored:
                break
    scored.sort(key=lambda x: (-x[0], x[1]["id"]))
    chosen = [site for _, site, _ in scored[:max_sites]]
    plan["sites"] = [s["id"] for s in chosen]
    plan["site_scores"] = {s["id"]: round(score, 1) for score, s, _ in scored[:max_sites]}
    return {**plan, "fallback": fallback, "selected": chosen}
