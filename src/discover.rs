use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, Utc};
use quick_xml::Reader;
use quick_xml::events::Event;
use regex::Regex;
use scraper::{Html, Selector};
use serde::Serialize;
use serde_json::Value;
use tokio::task::JoinSet;
use url::{Url, form_urlencoded};

use crate::http::{Fetcher, GetOpts};
use crate::index::{SiteIndex, canonical_url};
use crate::planner::{Method, Plan, Registry, Site, is_recall_query, plan_sites};
use crate::ranker::RankInput;
use crate::text::{
    fold, is_model_token, iso_date, long_ago, match_tokens, product_aliases, query_terms,
    ranker_text, slugify, stem_token, unslug,
};

const SITE_TIMEOUT_S: f64 = 8.0;
const OPEN_WEB_BUDGET_S: f64 = 8.0;
const OPEN_WEB_COOLDOWN_S: f64 = 600.0;
const SITEMAP_MEMORY_TTL_S: f64 = 3600.0;
const BODY_CHARS: usize = 4000;

const GENERIC_QUERY_TERMS: &[&str] = &[
    "recall",
    "recalls",
    "review",
    "reviews",
    "test",
    "tests",
    "best",
    "problem",
    "problems",
    "issue",
    "issues",
    "fix",
    "guide",
    "price",
    "buy",
    "vs",
    "comparison",
    "error",
    "errors",
    "code",
    "codes",
    "fault",
    "faults",
    "ariza",
    "sorun",
    "service",
    "servis",
    "warranty",
    "garanti",
    "replace",
    "replacement",
    "repair",
    "teardown",
    "tamir",
    "degisim",
    "yorum",
    "inceleme",
    "update",
    "updates",
    "and",
    "or",
    "the",
    "of",
    "for",
    "with",
    "about",
    "related",
    "information",
    "explain",
    "meaning",
    "definition",
];

const API_GENERIC: &[&str] = &[
    "recall", "recalls", "recalled", "review", "reviews", "test", "best", "vs", "problem",
    "problems", "issue", "issues", "fix", "guide", "price", "buy",
];

const PRODUCT_TERMS: &[&str] = &[
    "dishwasher",
    "washing",
    "washer",
    "dryer",
    "refrigerator",
    "fridge",
    "oven",
    "vacuum",
    "boiler",
    "kombi",
    "tv",
    "monitor",
    "laptop",
    "phone",
    "smartphone",
    "iphone",
    "ipad",
    "macbook",
    "airpods",
    "treadmill",
    "telefon",
    "buzdolabi",
    "camasir",
    "bulasik",
    "firin",
    "supurge",
    "araba",
    "arac",
    "machine",
    "makine",
    "oyuncak",
];

const BRAND_TERMS: &[&str] = &[
    "apple",
    "iphone",
    "ipad",
    "macbook",
    "airpods",
    "samsung",
    "lg",
    "sony",
    "anker",
    "dyson",
    "whirlpool",
    "bosch",
    "vaillant",
    "honda",
    "toyota",
    "ford",
    "tesla",
    "renault",
    "xiaomi",
    "redmi",
    "google",
    "pixel",
    "oneplus",
    "oppo",
    "vivo",
    "hyundai",
    "kia",
    "bmw",
    "mercedes",
    "audi",
    "volkswagen",
    "skoda",
    "dacia",
    "peugeot",
    "citroen",
    "opel",
    "fiat",
    "arcelik",
    "beko",
    "philips",
    "panasonic",
    "tcl",
    "hisense",
    "miele",
    "siemens",
    "zanussi",
    "insignia",
    "maytag",
    "kitchenaid",
    "ryzen",
    "nvidia",
    "amd",
    "intel",
    "sonos",
    "peloton",
    "steam",
    "baymak",
    "demirdokum",
    "eca",
    "buderus",
    "vestel",
    "altus",
    "regal",
    "tefal",
    "arzum",
    "fakir",
    "karaca",
];

const MULTI_SUFFIX: &[&str] = &[
    "co.uk", "com.tr", "gov.tr", "org.tr", "net.tr", "com.au", "co.jp", "co.nz", "com.br", "co.in",
    "com.mx", "com.sg",
];

#[derive(Clone, Debug)]
pub struct Candidate {
    pub url: String,
    pub title: String,
    pub snippet: String,
    pub published: Option<String>,
    pub source: String,
    pub method: String,
    pub score: f64,
    pub body: String,
}

#[derive(Clone, Debug)]
pub struct AdapterOutcome {
    pub site: String,
    pub method: String,
    pub status: String,
    pub candidates: Vec<Candidate>,
    pub fetched: usize,
    pub error: Option<String>,
}

pub type SitemapMem = Mutex<HashMap<String, (Instant, String, Vec<XmlEntry>)>>;

#[derive(Clone, Debug, Default)]
pub struct XmlEntry {
    pub loc: Option<String>,
    pub link: Option<String>,
    pub lastmod: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub pub_date: Option<String>,
    pub category: Option<String>,
    pub fold_text: String,
}

pub fn is_asset_url(url: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\.(?:jpe?g|png|gif|webp|svg|ico|css|js|woff2?|ttf|zip|gz|mp4|mp3|pdf)(?:$|\?)",
        )
        .unwrap()
    })
    .is_match(url)
}

pub fn base_domain(url_or_host: &str) -> String {
    let host = if url_or_host.contains("://") {
        Url::parse(url_or_host)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default()
    } else {
        url_or_host
            .split(':')
            .next()
            .unwrap_or("")
            .trim_start_matches('.')
            .to_ascii_lowercase()
    };
    let labels: Vec<&str> = host.split('.').filter(|s| !s.is_empty()).collect();
    if labels.len() <= 2 {
        return host;
    }
    let last2 = format!("{}.{}", labels[labels.len() - 2], labels[labels.len() - 1]);
    if MULTI_SUFFIX.contains(&last2.as_str()) && labels.len() >= 3 {
        labels[labels.len() - 3..].join(".")
    } else {
        last2
    }
}

fn local_name(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    s.rsplit(':').next().unwrap_or(&s).to_ascii_lowercase()
}

pub fn parse_xml(text: &str) -> (String, Vec<XmlEntry>) {
    let clean = text.trim_start_matches(|c: char| c == '\u{feff}' || c.is_whitespace());
    if clean.is_empty() {
        return (String::new(), Vec::new());
    }
    let mut reader = Reader::from_str(clean);
    reader.config_mut().trim_text(true);
    let mut root_kind = String::new();
    let mut entries = Vec::new();
    let mut in_item = false;
    let mut cur = XmlEntry::default();
    let mut cur_tag = String::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let tag = local_name(e.name().as_ref());
                if root_kind.is_empty() {
                    root_kind = tag.clone();
                } else if (root_kind == "sitemapindex" && tag == "sitemap")
                    || (root_kind == "urlset" && tag == "url")
                    || (root_kind == "rss" && tag == "item")
                    || (root_kind == "feed" && tag == "entry")
                {
                    in_item = true;
                    cur = XmlEntry::default();
                } else if in_item {
                    if root_kind == "feed" && tag == "link" {
                        let mut href = None;
                        let mut rel = String::from("alternate");
                        for attr in e.attributes().flatten() {
                            let k = local_name(attr.key.as_ref());
                            if let Ok(v) = attr.unescape_value() {
                                if k == "href" {
                                    href = Some(v.into_owned());
                                } else if k == "rel" {
                                    rel = v.into_owned();
                                }
                            }
                        }
                        if rel == "alternate" && href.is_some() {
                            cur.link = href;
                        }
                    }
                    cur_tag = tag;
                }
            }
            Ok(Event::Empty(e)) => {
                let tag = local_name(e.name().as_ref());
                if in_item && root_kind == "feed" && tag == "link" {
                    let mut href = None;
                    let mut rel = String::from("alternate");
                    for attr in e.attributes().flatten() {
                        let k = local_name(attr.key.as_ref());
                        if let Ok(v) = attr.unescape_value() {
                            if k == "href" {
                                href = Some(v.into_owned());
                            } else if k == "rel" {
                                rel = v.into_owned();
                            }
                        }
                    }
                    if rel == "alternate" && href.is_some() {
                        cur.link = href;
                    }
                }
            }
            Ok(Event::Text(e)) => {
                if in_item
                    && !cur_tag.is_empty()
                    && let Ok(v) = e.unescape()
                {
                    assign_xml_field(&root_kind, &cur_tag, v.trim(), &mut cur);
                }
            }
            Ok(Event::CData(e)) => {
                if in_item && !cur_tag.is_empty() {
                    let v = String::from_utf8_lossy(e.as_ref());
                    assign_xml_field(&root_kind, &cur_tag, v.trim(), &mut cur);
                }
            }
            Ok(Event::End(e)) => {
                let tag = local_name(e.name().as_ref());
                if in_item
                    && ((root_kind == "sitemapindex" && tag == "sitemap")
                        || (root_kind == "urlset" && tag == "url")
                        || (root_kind == "rss" && tag == "item")
                        || (root_kind == "feed" && tag == "entry"))
                {
                    in_item = false;
                    cur_tag.clear();
                    finalize_xml_entry(&root_kind, cur, &mut entries);
                    cur = XmlEntry::default();
                } else {
                    cur_tag.clear();
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => return (String::new(), Vec::new()),
            _ => {}
        }
        buf.clear();
    }
    (root_kind, entries)
}

fn assign_xml_field(root_kind: &str, tag: &str, val: &str, cur: &mut XmlEntry) {
    if val.is_empty() {
        return;
    }
    match (root_kind, tag) {
        (_, "loc") => cur.loc = Some(val.to_string()),
        (_, "lastmod") => cur.lastmod = Some(val.to_string()),
        (_, "title") => cur.title = Some(val.to_string()),
        ("rss", "link") => cur.link = Some(val.to_string()),
        ("rss", "description") => cur.description = Some(val.to_string()),
        ("rss", "pubdate") => cur.pub_date = Some(val.to_string()),
        ("rss", "category") => cur.category = Some(val.to_string()),
        ("feed", "summary") => cur.description = Some(val.to_string()),
        ("feed", "published") | ("feed", "updated") => {
            if cur.pub_date.is_none() {
                cur.pub_date = Some(val.to_string());
            }
        }
        ("feed", "id") => {
            if cur.link.is_none() {
                cur.link = Some(val.to_string());
            }
        }
        ("feed", "link") if cur.link.is_none() => {
            cur.link = Some(val.to_string());
        }
        _ => {}
    }
}

fn finalize_xml_entry(root_kind: &str, mut cur: XmlEntry, out: &mut Vec<XmlEntry>) {
    match root_kind {
        "sitemapindex" => {
            if cur.loc.is_some() {
                out.push(cur);
            }
        }
        "urlset" => {
            if let Some(ref loc) = cur.loc {
                cur.fold_text = fold(&unslug(loc));
                out.push(cur);
            }
        }
        "rss" | "feed" if cur.link.is_some() => {
            cur.fold_text = fold(&format!(
                "{} {} {}",
                cur.title.as_deref().unwrap_or(""),
                cur.description.as_deref().unwrap_or(""),
                cur.category.as_deref().unwrap_or("")
            ));
            out.push(cur);
        }
        _ => {}
    }
}

pub fn select_sitemap_children(
    entries: &[XmlEntry],
    method: &Method,
    limit: usize,
) -> Vec<XmlEntry> {
    static DATE_RE: OnceLock<Regex> = OnceLock::new();
    let date_re =
        DATE_RE.get_or_init(|| Regex::new(r"(19\d{2}|20\d{2})[-_](\d{1,2})(?:[^0-9]|$)").unwrap());
    let include: Vec<Regex> = method
        .include_children
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();
    let exclude: Vec<Regex> = method
        .exclude_children
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect();
    let mut selected = Vec::new();
    for item in entries {
        let loc = item.loc.as_deref().unwrap_or("");
        if !include.is_empty() && !include.iter().any(|r| r.is_match(loc)) {
            continue;
        }
        if !exclude.is_empty() && exclude.iter().any(|r| r.is_match(loc)) {
            continue;
        }
        selected.push(item.clone());
    }
    selected.sort_by(|a, b| {
        let key = |it: &XmlEntry| -> String {
            let loc = it.loc.as_deref().unwrap_or("");
            if let Some(cap) = date_re.captures(loc) {
                let y = &cap[1];
                let m: u32 = cap[2].parse().unwrap_or(0);
                format!("{y}-{m:02}")
            } else {
                it.lastmod.clone().unwrap_or_default()
            }
        };
        key(b).cmp(&key(a))
    });
    if limit > 0 {
        selected.truncate(limit);
    }
    selected
}

fn generic_set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| GENERIC_QUERY_TERMS.iter().copied().collect())
}

pub fn score_candidates(
    query: &str,
    candidates: Vec<Candidate>,
    max_results: usize,
) -> Vec<Candidate> {
    if candidates.is_empty() {
        return Vec::new();
    }
    let gen_set = generic_set();
    let raw_terms: Vec<String> = query_terms(query, false)
        .into_iter()
        .filter(|t| !gen_set.contains(t.as_str()))
        .collect();
    let mut terms: Vec<String> = Vec::new();
    for t in &raw_terms {
        let st = stem_token(t);
        if !terms.contains(&st) {
            terms.push(st);
        }
    }
    for term in terms.clone() {
        if term == "upgrade" {
            for syn in ["replacement", "replace"] {
                if !terms.iter().any(|t| t == syn) {
                    terms.push(syn.to_string());
                }
            }
        }
        for alias in product_aliases(&term) {
            if !terms.contains(&alias) {
                terms.push(alias);
            }
        }
    }
    if terms.is_empty() {
        return Vec::new();
    }
    let alpha_terms: Vec<String> = terms
        .iter()
        .filter(|t| t.bytes().any(|b| b.is_ascii_alphabetic()))
        .cloned()
        .collect();
    let specific_terms: Vec<String> = terms
        .iter()
        .filter(|t| !gen_set.contains(t.as_str()))
        .cloned()
        .collect();

    let texts: Vec<HashSet<String>> = candidates
        .iter()
        .map(|c| match_tokens(&format!("{} {}", c.title, c.snippet)))
        .collect();
    let n = candidates.len() as f64;
    let mut weights: HashMap<&str, f64> = HashMap::new();
    for term in &terms {
        let df = texts
            .iter()
            .filter(|toks| toks.contains(term.as_str()))
            .count() as f64;
        let mut w = if df == 0.0 {
            0.0
        } else {
            (1.0 + n / (1.0 + df)).ln()
        };
        if term.bytes().all(|b| b.is_ascii_digit()) {
            w *= 0.7;
        } else if is_model_token(term) {
            w *= 2.5;
        }
        weights.insert(term.as_str(), w);
    }
    let total: f64 = {
        let s: f64 = weights.values().sum();
        if s > 0.0 { s } else { 1.0 }
    };

    let qfold = fold(query);
    let qterms = query_terms(query, false);
    let qcompounds: HashSet<String> = qterms
        .windows(2)
        .filter_map(|w| {
            let c = format!("{}{}", w[0], w[1]);
            qterms.contains(&c).then_some(c)
        })
        .collect();
    let qwords: Vec<String> = qterms
        .iter()
        .filter(|t| !gen_set.contains(t.as_str()) && !qcompounds.contains(t.as_str()))
        .cloned()
        .collect();
    let query_phrase = if qwords.len() >= 2 {
        qwords.join(" ")
    } else {
        String::new()
    };
    let numeric_terms: Vec<String> = terms
        .iter()
        .filter(|t| t.bytes().all(|b| b.is_ascii_digit()))
        .cloned()
        .collect();

    static SEP_RE: OnceLock<Regex> = OnceLock::new();
    let sep_re = SEP_RE.get_or_init(|| Regex::new(r"[-_/]+").unwrap());

    let min_specific = if specific_terms.len() >= 2 { 2 } else { 1 };
    let mut scored: Vec<(Candidate, f64, usize, bool)> = Vec::new();
    for (mut cand, toks) in candidates.into_iter().zip(texts.iter()) {
        let matched: Vec<&str> = terms
            .iter()
            .filter(|t| weights[t.as_str()] > 0.0 && toks.contains(t.as_str()))
            .map(|t| t.as_str())
            .collect();
        if matched.is_empty() {
            continue;
        }
        if !alpha_terms.is_empty() && !alpha_terms.iter().any(|t| matched.contains(&t.as_str())) {
            continue;
        }
        let spec_matched: Vec<&str> = matched
            .iter()
            .copied()
            .filter(|t| specific_terms.iter().any(|s| s == t))
            .collect();
        if !specific_terms.is_empty() && spec_matched.is_empty() {
            continue;
        }
        let mut score: f64 = matched.iter().map(|t| weights[t]).sum::<f64>() / total;
        if score <= 0.0 {
            continue;
        }
        let raw = fold(&format!("{} {} {}", cand.url, cand.title, cand.snippet));
        if !query_phrase.is_empty() {
            let flat = sep_re.replace_all(&raw, " ");
            if flat.contains(&query_phrase) {
                score += 0.30;
            }
            let folded_title = fold(&cand.title);
            let flat_title = sep_re.replace_all(&folded_title, " ");
            if flat_title.contains(&query_phrase) {
                score += 0.30;
            }
        }
        if (qfold.contains("review") || qfold.contains("inceleme"))
            && (raw.contains("review") || raw.contains("inceleme"))
        {
            score += 0.25;
        }
        if (qfold.contains("fiyat") || qfold.contains("price"))
            && (raw.contains("fiyat") || raw.contains("price"))
        {
            score += 0.25;
        }
        if qfold.contains("recall") && raw.contains("recall") {
            score += 0.10;
        }
        if ["repair", "replacement", "replace", "tamir", "degisim"]
            .iter()
            .any(|t| qfold.contains(t))
            && [
                "repair",
                "replacement",
                "replace",
                "tamir",
                "degisim",
                "guide",
            ]
            .iter()
            .any(|t| raw.contains(t))
        {
            score += 0.15;
        }
        if !numeric_terms.is_empty() && numeric_terms.iter().any(|t| toks.contains(t.as_str())) {
            score += 0.10;
        }
        if cand.method == "json_search"
            && cand.snippet.trim().is_empty()
            && cand.body.trim().is_empty()
        {
            score = (score - 0.25).max(0.05);
        }
        cand.score = (score * 10000.0).round() / 10000.0;
        let has_model = spec_matched.iter().any(|t| is_model_token(t));
        scored.push((cand, score, spec_matched.len(), has_model));
    }

    let strict: Vec<_> = scored
        .iter()
        .filter(|x| x.2 >= min_specific)
        .cloned()
        .collect();
    let mut chosen = if !strict.is_empty() {
        strict
    } else if scored.iter().any(|x| x.3)
        || specific_terms
            .iter()
            .any(|t| is_model_token(t) && weights.get(t.as_str()).copied().unwrap_or(0.0) == 0.0)
    {
        scored
    } else {
        Vec::new()
    };

    chosen.sort_by(|a, b| {
        b.1.total_cmp(&a.1).then(b.2.cmp(&a.2)).then_with(|| {
            b.0.published
                .as_deref()
                .unwrap_or("")
                .cmp(a.0.published.as_deref().unwrap_or(""))
        })
    });
    chosen
        .into_iter()
        .take(max_results)
        .map(|(c, ..)| c)
        .collect()
}

pub fn enforce_model_terms(query: &str, candidates: Vec<Candidate>) -> Vec<Candidate> {
    let gen_set = generic_set();
    let qterms = query_terms(query, false);
    let model_terms: Vec<String> = qterms
        .iter()
        .filter(|t| is_model_token(t))
        .cloned()
        .collect();
    if model_terms.is_empty() {
        return candidates;
    }
    let context_terms: Vec<String> = qterms
        .into_iter()
        .filter(|t| !model_terms.contains(t) && !gen_set.contains(t.as_str()))
        .collect();
    let tokens_of = |c: &Candidate| match_tokens(&format!("{} {} {}", c.url, c.title, c.snippet));
    let has_model = |toks: &HashSet<String>| model_terms.iter().any(|t| toks.contains(t.as_str()));

    if !context_terms.is_empty() {
        let both: Vec<Candidate> = candidates
            .iter()
            .filter(|c| {
                let toks = tokens_of(c);
                has_model(&toks) && context_terms.iter().any(|t| toks.contains(t.as_str()))
            })
            .cloned()
            .collect();
        if !both.is_empty() {
            return both;
        }
    }
    let model_only: Vec<Candidate> = candidates
        .iter()
        .filter(|c| has_model(&tokens_of(c)))
        .cloned()
        .collect();
    if model_only.is_empty() {
        candidates
    } else {
        model_only
    }
}

pub fn enforce_brand_terms(query: &str, candidates: Vec<Candidate>) -> Vec<Candidate> {
    let brand_tokens: Vec<String> = query_terms(query, false)
        .into_iter()
        .filter(|t| BRAND_TERMS.contains(&t.as_str()))
        .map(|b| stem_token(&b))
        .collect();
    if brand_tokens.is_empty() {
        return candidates;
    }
    candidates
        .into_iter()
        .filter(|c| {
            let toks = match_tokens(&format!("{} {} {}", c.url, c.title, c.snippet));
            brand_tokens.iter().any(|b| toks.contains(b.as_str()))
        })
        .collect()
}

pub fn enforce_product_terms(query: &str, candidates: Vec<Candidate>) -> Vec<Candidate> {
    let products: Vec<String> = query_terms(query, false)
        .into_iter()
        .filter(|t| PRODUCT_TERMS.contains(&t.as_str()))
        .collect();
    if products.is_empty() {
        return candidates;
    }
    let mut product_tokens = HashSet::new();
    for p in products {
        for a in product_aliases(&p) {
            product_tokens.insert(a);
        }
    }
    candidates
        .into_iter()
        .filter(|c| {
            let toks = match_tokens(&format!("{} {} {}", c.url, c.title, c.snippet));
            product_tokens.iter().any(|p| toks.contains(p.as_str()))
        })
        .collect()
}

pub fn enforce_intent_terms(query: &str, candidates: Vec<Candidate>) -> Vec<Candidate> {
    let q = fold(query);
    let markers: &[&str] = if q.contains("fiyat") || q.contains("price") {
        &["fiyat", "price"]
    } else if q.contains("review") || q.contains("inceleme") {
        &["review", "inceleme", "faceoff", "handson", "hands-on"]
    } else {
        return candidates;
    };
    let matched: Vec<Candidate> = candidates
        .iter()
        .filter(|c| {
            let txt = fold(&format!("{} {}", c.url, c.title));
            markers.iter().any(|m| txt.contains(m))
        })
        .cloned()
        .collect();
    if matched.is_empty() {
        candidates
    } else {
        matched
    }
}

fn collapse_paginated(candidates: Vec<Candidate>) -> Vec<Candidate> {
    static PAGE_SUFFIX: OnceLock<Regex> = OnceLock::new();
    let re = PAGE_SUFFIX.get_or_init(|| Regex::new(r"^(.*?)/([2-9])$").unwrap());
    let urls: HashSet<String> = candidates.iter().map(|c| canonical_url(&c.url)).collect();
    candidates
        .into_iter()
        .filter(|c| {
            if let Some(cap) = re.captures(&c.url) {
                !urls.contains(&canonical_url(&cap[1]))
            } else {
                true
            }
        })
        .collect()
}

fn latest_year(query: &str, candidates: Vec<Candidate>, topics: &[String]) -> Vec<Candidate> {
    static YEAR_RE: OnceLock<Regex> = OnceLock::new();
    let re = YEAR_RE.get_or_init(|| Regex::new(r"\b(19\d{2}|20\d{2})\b").unwrap());
    if !topics.iter().any(|t| t == "cars")
        || topics.iter().any(|t| t == "recalls" || t == "complaints")
        || re.is_match(query)
    {
        return candidates;
    }
    let years_of = |c: &Candidate| -> Vec<i32> {
        let s = format!("{} {}", c.url, c.title);
        re.find_iter(&s)
            .filter_map(|m| m.as_str().parse::<i32>().ok())
            .collect()
    };
    let all_max: Vec<i32> = candidates
        .iter()
        .filter_map(|c| years_of(c).into_iter().max())
        .collect();
    let Some(&max_y) = all_max.iter().max() else {
        return candidates;
    };
    let cur_year = Utc::now().year();
    let newest = max_y.min(cur_year);
    if newest < 2020 {
        return candidates;
    }
    let kept: Vec<Candidate> = candidates
        .iter()
        .filter(|c| {
            let ys = years_of(c);
            if newest >= cur_year - 1 {
                ys.into_iter()
                    .max()
                    .map(|y| y >= newest - 1)
                    .unwrap_or(false)
            } else {
                ys.into_iter()
                    .max()
                    .map(|y| y >= newest - 1)
                    .unwrap_or(true)
            }
        })
        .cloned()
        .collect();
    if kept.is_empty() { candidates } else { kept }
}

fn base_review_url(url: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)(-(?:handson-)?review-\d+)p\d+(\.php)$").unwrap());
    re.replace(url, "$1$2").into_owned()
}

fn strip_html(value: &str) -> String {
    static TAG_RE: OnceLock<Regex> = OnceLock::new();
    let tag_re = TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    let no_tags = tag_re.replace_all(value, " ");
    let decoded = no_tags
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn q_encode(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn path_encode_wiki(s: &str) -> String {
    let replaced = s.replace(' ', "_");
    url::form_urlencoded::byte_serialize(replaced.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

fn dig<'a>(data: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = data;
    for part in path.split('.').filter(|p| !p.is_empty()) {
        cur = match cur {
            Value::Array(arr) => arr.get(part.parse::<usize>().ok()?)?,
            Value::Object(map) => map.get(part)?,
            _ => return None,
        };
    }
    Some(cur)
}

fn scalar_str(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .first()
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn iso_any(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::Number(n) => {
            let ts = n.as_f64()? as i64;
            DateTime::<Utc>::from_timestamp(ts, 0).map(|d| d.to_rfc3339())
        }
        Value::String(s) => iso_date(Some(s)),
        _ => None,
    }
}

async fn load_xml(fetcher: &Fetcher, url: &str, mem: &SitemapMem) -> (String, Vec<XmlEntry>) {
    {
        let guard = mem.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, kind, entries)) = guard.get(url)
            && at.elapsed().as_secs_f64() < SITEMAP_MEMORY_TTL_S
        {
            return (kind.clone(), entries.clone());
        }
    }
    let res = fetcher.get(url, GetOpts::default()).await;
    if !res.ok() {
        return (String::new(), Vec::new());
    }
    let (kind, entries) = parse_xml(&res.text);
    if !kind.is_empty() {
        mem.lock().unwrap_or_else(PoisonError::into_inner).insert(
            url.to_string(),
            (Instant::now(), kind.clone(), entries.clone()),
        );
    }
    (kind, entries)
}

pub async fn run_site(
    site: &Site,
    query: &str,
    fetcher: &Fetcher,
    sitemap_mem: &SitemapMem,
) -> Vec<AdapterOutcome> {
    let mut outcomes = Vec::new();
    for method in &site.methods {
        let outcome = match method.kind.as_str() {
            "sitemap" => run_sitemap(site, method, query, fetcher, sitemap_mem).await,
            "rss" => run_rss(site, method, query, fetcher, sitemap_mem).await,
            "search" => run_search(site, method, query, fetcher).await,
            "api" => run_api(site, method, query, fetcher).await,
            "json_search" => run_json_search(site, method, query, fetcher).await,
            "url_pattern" => run_url_pattern(site, method, query),
            _ => continue,
        };
        let has_cands = !outcome.candidates.is_empty();
        let robots_denied = outcome.status == "robots-denied";
        outcomes.push(outcome);
        if has_cands {
            break;
        }
        if robots_denied {
            continue;
        }
    }
    outcomes
}

async fn run_sitemap(
    site: &Site,
    method: &Method,
    query: &str,
    fetcher: &Fetcher,
    sitemap_mem: &SitemapMem,
) -> AdapterOutcome {
    let max_sitemaps = method.max_sitemaps.unwrap_or(4);
    let max_urls = method.max_urls.unwrap_or(20_000);
    let max_results = method.max_results.unwrap_or(40);
    let mut index_urls = method.urls.clone();
    if index_urls.is_empty()
        && let Some(ref u) = method.url
    {
        index_urls.push(u.clone());
    }

    let mut entries = Vec::new();
    let mut fetched = 0usize;
    let mut statuses = Vec::new();

    for url in index_urls {
        let res = fetcher.get(&url, GetOpts::default()).await;
        fetched += 1;
        if res.robots_skipped {
            statuses.push("robots-denied".to_string());
            continue;
        }
        if !res.ok() {
            statuses.push(res.status_tag);
            continue;
        }
        let (kind, items) = load_xml(fetcher, &url, sitemap_mem).await;
        statuses.push(if kind.is_empty() {
            "unparsed".into()
        } else {
            kind.clone()
        });
        if kind == "sitemapindex" {
            let children = select_sitemap_children(&items, method, max_sitemaps);
            for child in children {
                if let Some(cloc) = child.loc {
                    let (ckind, citems) = load_xml(fetcher, &cloc, sitemap_mem).await;
                    fetched += 1;
                    if ckind == "urlset" || ckind == "rss" {
                        entries.extend(citems);
                    }
                }
            }
        } else if kind == "urlset" || kind == "rss" {
            entries.extend(items);
        }
    }

    if !statuses.is_empty() && statuses.iter().all(|s| s == "robots-denied") {
        return AdapterOutcome {
            site: site.id.clone(),
            method: "sitemap".into(),
            status: "robots-denied".into(),
            candidates: Vec::new(),
            fetched,
            error: None,
        };
    }

    let domain = site.domain.to_ascii_lowercase();
    let terms: Vec<String> = query_terms(query, false)
        .into_iter()
        .map(|t| stem_token(&t))
        .collect();
    let mut need = if terms.len() >= 3 { 2 } else { 1 };
    if need == 2 {
        let any_two = entries.iter().any(|it| {
            let txt = if it.fold_text.is_empty() {
                fold(&unslug(it.loc.as_deref().unwrap_or("")))
            } else {
                it.fold_text.clone()
            };
            terms.iter().filter(|t| txt.contains(t.as_str())).count() >= 2
        });
        if !any_two {
            need = 1;
        }
    }

    let mut cands = Vec::new();
    let mut seen = HashSet::new();
    for item in entries {
        let url = item.loc.as_deref().or(item.link.as_deref()).unwrap_or("");
        if url.is_empty() || seen.contains(url) {
            continue;
        }
        let txt = if item.fold_text.is_empty() {
            fold(&unslug(url))
        } else {
            item.fold_text
        };
        if terms.iter().filter(|t| txt.contains(t.as_str())).count() < need {
            continue;
        }
        seen.insert(url.to_string());
        if is_asset_url(url) {
            continue;
        }
        let host = Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default();
        if !host.contains(&domain) {
            continue;
        }
        let title = item.title.unwrap_or_else(|| unslug(url));
        let snippet = format!("{} {}", site.name, unslug(url));
        cands.push(Candidate {
            url: url.to_string(),
            title: title.chars().take(300).collect(),
            snippet: snippet.chars().take(500).collect(),
            published: iso_date(item.lastmod.as_deref().or(item.pub_date.as_deref())),
            source: site.id.clone(),
            method: "sitemap".into(),
            score: 0.0,
            body: String::new(),
        });
        if cands.len() >= max_urls {
            break;
        }
    }
    let ranked = score_candidates(query, cands, max_results);
    AdapterOutcome {
        site: site.id.clone(),
        method: "sitemap".into(),
        status: if ranked.is_empty() {
            "no-results"
        } else {
            "ok"
        }
        .into(),
        candidates: ranked,
        fetched,
        error: None,
    }
}

async fn run_rss(
    site: &Site,
    method: &Method,
    query: &str,
    fetcher: &Fetcher,
    sitemap_mem: &SitemapMem,
) -> AdapterOutcome {
    let url = method
        .url
        .as_deref()
        .unwrap_or("")
        .replace("{query}", &q_encode(query))
        .replace("{slug}", &slugify(query));
    let res = fetcher.get(&url, GetOpts::default()).await;
    if res.robots_skipped {
        return AdapterOutcome {
            site: site.id.clone(),
            method: "rss".into(),
            status: "robots-denied".into(),
            candidates: Vec::new(),
            fetched: 1,
            error: None,
        };
    }
    if !res.ok() {
        return AdapterOutcome {
            site: site.id.clone(),
            method: "rss".into(),
            status: "error".into(),
            candidates: Vec::new(),
            fetched: 1,
            error: Some(format!("HTTP {}", res.status_tag)),
        };
    }
    let (kind, items) = load_xml(fetcher, &url, sitemap_mem).await;
    if kind != "rss" && kind != "feed" {
        return AdapterOutcome {
            site: site.id.clone(),
            method: "rss".into(),
            status: "error".into(),
            candidates: Vec::new(),
            fetched: 1,
            error: Some("not an RSS/Atom feed".into()),
        };
    }
    let max_results = method.max_results.unwrap_or(20);
    let mut cands = Vec::new();
    for item in items {
        let Some(link) = item.link else { continue };
        let title = strip_html(item.title.as_deref().unwrap_or(""));
        let desc = strip_html(item.description.as_deref().unwrap_or(""));
        let cat = strip_html(item.category.as_deref().unwrap_or(""));
        let display = if cat.is_empty() {
            title
        } else {
            format!("{cat}: {title}")
        };
        cands.push(Candidate {
            url: link,
            title: display.chars().take(300).collect(),
            snippet: desc.chars().take(500).collect(),
            published: iso_date(item.pub_date.as_deref()),
            source: site.id.clone(),
            method: "rss".into(),
            score: 0.0,
            body: String::new(),
        });
    }
    let ranked = score_candidates(query, cands, max_results);
    AdapterOutcome {
        site: site.id.clone(),
        method: "rss".into(),
        status: if ranked.is_empty() {
            "no-results"
        } else {
            "ok"
        }
        .into(),
        candidates: ranked,
        fetched: 1,
        error: None,
    }
}

async fn run_search(
    site: &Site,
    method: &Method,
    query: &str,
    fetcher: &Fetcher,
) -> AdapterOutcome {
    let url = method
        .url
        .as_deref()
        .unwrap_or("")
        .replace("{query}", &q_encode(query));
    let res = fetcher.get(&url, GetOpts::default()).await;
    if res.robots_skipped {
        return AdapterOutcome {
            site: site.id.clone(),
            method: "search".into(),
            status: "robots-denied".into(),
            candidates: Vec::new(),
            fetched: 1,
            error: None,
        };
    }
    if !res.ok() {
        return AdapterOutcome {
            site: site.id.clone(),
            method: "search".into(),
            status: "error".into(),
            candidates: Vec::new(),
            fetched: 1,
            error: Some(format!("HTTP {}", res.status_tag)),
        };
    }
    let selector_str = method.link_selector.as_deref().unwrap_or("a[href]");
    let Ok(selector) = Selector::parse(selector_str) else {
        return AdapterOutcome {
            site: site.id.clone(),
            method: "search".into(),
            status: "error".into(),
            candidates: Vec::new(),
            fetched: 1,
            error: Some("bad css selector".into()),
        };
    };
    let base_url = Url::parse(method.base_url.as_deref().unwrap_or(&url)).ok();
    let max_results = method.max_results.unwrap_or(20);
    let domain = site.domain.to_ascii_lowercase();

    let mut cands = Vec::new();
    let mut seen = HashSet::new();
    {
        let doc = Html::parse_document(&res.text);
        for a in doc.select(&selector) {
            let href = a.value().attr("href").unwrap_or("").trim();
            if href.is_empty() || href.starts_with('#') || href.starts_with("javascript:") {
                continue;
            }
            let link = match base_url.as_ref().and_then(|b| b.join(href).ok()) {
                Some(u) => u.to_string(),
                None => continue,
            };
            let host = Url::parse(&link)
                .ok()
                .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
                .unwrap_or_default();
            if !host.contains(&domain) || is_asset_url(&link) || !seen.insert(link.clone()) {
                continue;
            }
            let title_raw = a.text().collect::<Vec<_>>().join(" ");
            let title = {
                let t = title_raw.split_whitespace().collect::<Vec<_>>().join(" ");
                if !t.is_empty() {
                    t
                } else {
                    a.value()
                        .attr("title")
                        .map(str::to_string)
                        .unwrap_or_else(|| unslug(&link))
                }
            };
            cands.push(Candidate {
                url: link,
                title: title.chars().take(300).collect(),
                snippet: title.chars().take(500).collect(),
                published: None,
                source: site.id.clone(),
                method: "search".into(),
                score: 0.0,
                body: String::new(),
            });
            if cands.len() >= max_results * 3 {
                break;
            }
        }
    }
    let ranked = score_candidates(query, cands, max_results);
    AdapterOutcome {
        site: site.id.clone(),
        method: "search".into(),
        status: if ranked.is_empty() {
            "no-results"
        } else {
            "ok"
        }
        .into(),
        candidates: ranked,
        fetched: 1,
        error: None,
    }
}

async fn run_api(site: &Site, method: &Method, query: &str, fetcher: &Fetcher) -> AdapterOutcome {
    let template = method.url.as_deref().unwrap_or("");
    let mut product_terms: Vec<String> = query_terms(query, false)
        .into_iter()
        .filter(|t| !API_GENERIC.contains(&t.as_str()))
        .collect();
    if product_terms.iter().any(|t| t == "treadmill") {
        product_terms.push("tread".to_string());
    }
    let mut probes = vec![query.to_string()];
    for t in product_terms.into_iter().take(3) {
        if !probes.contains(&t) {
            probes.push(t);
        }
    }
    probes.truncate(method.probes.unwrap_or(4));
    let max_results = method.max_results.unwrap_or(20);
    let mut cands = Vec::new();
    let mut seen = HashSet::new();
    let mut fetched = 0usize;

    for probe in probes {
        let url = template.replace("{query}", &q_encode(&probe));
        let res = fetcher.get(&url, GetOpts::default()).await;
        fetched += 1;
        if res.robots_skipped || !res.ok() {
            continue;
        }
        let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&res.text) else {
            continue;
        };
        for item in items {
            let link = item["URL"].as_str().unwrap_or("").trim().to_string();
            if link.is_empty() || !seen.insert(link.clone()) {
                continue;
            }
            let title = item["Title"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| unslug(&link));
            let desc = item["Description"].as_str().unwrap_or("").trim();
            let pub_str = item["RecallDate"]
                .as_str()
                .or_else(|| item["LastPublishDate"].as_str());
            cands.push(Candidate {
                url: link,
                title: title.chars().take(300).collect(),
                snippet: desc.chars().take(600).collect(),
                published: iso_date(pub_str),
                source: site.id.clone(),
                method: "api".into(),
                score: 0.0,
                body: String::new(),
            });
        }
    }
    let ranked = score_candidates(query, cands, max_results);
    AdapterOutcome {
        site: site.id.clone(),
        method: "api".into(),
        status: if ranked.is_empty() {
            "no-results"
        } else {
            "ok"
        }
        .into(),
        candidates: ranked,
        fetched,
        error: None,
    }
}

async fn run_json_search(
    site: &Site,
    method: &Method,
    query: &str,
    fetcher: &Fetcher,
) -> AdapterOutcome {
    let template = method.url.as_deref().unwrap_or("");
    let mut probes = vec![query.to_string()];
    for t in query_terms(query, false)
        .into_iter()
        .filter(|t| !API_GENERIC.contains(&t.as_str()))
        .take(2)
    {
        if !probes.contains(&t) {
            probes.push(t);
        }
    }
    probes.truncate(method.probes.unwrap_or(2));
    let max_results = method.max_results.unwrap_or(20);
    let timeout = method.timeout.unwrap_or(20.0);
    let mut cands = Vec::new();
    let mut seen = HashSet::new();
    let mut fetched = 0usize;

    for probe in probes {
        let url = template
            .replace("{query}", &q_encode(&probe))
            .replace("{slug}", &slugify(&probe));
        let res = fetcher
            .get(
                &url,
                GetOpts {
                    timeout_s: timeout,
                    ..Default::default()
                },
            )
            .await;
        fetched += 1;
        if res.robots_skipped || !res.ok() {
            continue;
        }
        let Ok(data) = serde_json::from_str::<Value>(&res.text) else {
            continue;
        };
        let Some(Value::Array(items)) = dig(&data, method.items_path.as_deref().unwrap_or(""))
        else {
            continue;
        };
        for item in items {
            let mut link = scalar_str(dig(item, method.url_field.as_deref().unwrap_or("url")));
            if let Some(ref tpl) = method.url_template {
                link = tpl.replace("{value}", &path_encode_wiki(&link));
            }
            if !link.starts_with("http") || !seen.insert(link.clone()) {
                continue;
            }
            let raw_title = scalar_str(dig(item, method.title.as_deref().unwrap_or("title")));
            let title = {
                let s = strip_html(&raw_title);
                if s.is_empty() { unslug(&link) } else { s }
            };
            let snippet = strip_html(&scalar_str(dig(
                item,
                method.snippet.as_deref().unwrap_or("snippet"),
            )));
            let body = if let Some(ref bf) = method.body_field {
                strip_html(&scalar_str(dig(item, bf)))
                    .chars()
                    .take(BODY_CHARS)
                    .collect()
            } else {
                String::new()
            };
            let pub_date = method.date.as_deref().and_then(|dp| iso_any(dig(item, dp)));
            cands.push(Candidate {
                url: link,
                title: title.chars().take(300).collect(),
                snippet: snippet.chars().take(600).collect(),
                published: pub_date,
                source: site.id.clone(),
                method: "json_search".into(),
                score: 0.0,
                body,
            });
        }
    }
    let ranked = score_candidates(query, cands, max_results);
    AdapterOutcome {
        site: site.id.clone(),
        method: "json_search".into(),
        status: if ranked.is_empty() {
            "no-results"
        } else {
            "ok"
        }
        .into(),
        candidates: ranked,
        fetched,
        error: None,
    }
}

fn run_url_pattern(site: &Site, method: &Method, query: &str) -> AdapterOutcome {
    let terms = query_terms(query, false);
    if let Some(limit) = method.max_query_terms
        && terms.len() > limit
    {
        return AdapterOutcome {
            site: site.id.clone(),
            method: "url_pattern".into(),
            status: "no-results".into(),
            candidates: Vec::new(),
            fetched: 0,
            error: None,
        };
    }
    let slug = slugify(query);
    let mut cands = Vec::new();
    for tpl in &method.templates {
        let u = tpl
            .replace("{slug}", &slug)
            .replace("{query}", &q_encode(query));
        cands.push(Candidate {
            url: u.clone(),
            title: unslug(&u),
            snippet: format!("{} {}", site.name, slug),
            published: None,
            source: site.id.clone(),
            method: "url_pattern".into(),
            score: 0.0,
            body: String::new(),
        });
    }
    let ranked = score_candidates(query, cands, method.max_results.unwrap_or(5));
    AdapterOutcome {
        site: site.id.clone(),
        method: "url_pattern".into(),
        status: if ranked.is_empty() {
            "no-results"
        } else {
            "ok"
        }
        .into(),
        candidates: ranked,
        fetched: 0,
        error: None,
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct BreakerEntry {
    pub failures: usize,
    pub open: bool,
    pub trips: usize,
}

#[derive(Default)]
struct BreakerInner {
    failures: usize,
    opened_at: Option<Instant>,
    trips: usize,
}

pub struct CircuitBreaker {
    threshold: usize,
    cooldown: Duration,
    state: Mutex<HashMap<String, BreakerInner>>,
}

impl CircuitBreaker {
    pub fn new(threshold: usize, cooldown_s: f64) -> Self {
        Self {
            threshold,
            cooldown: Duration::from_secs_f64(cooldown_s),
            state: Mutex::new(HashMap::new()),
        }
    }

    pub fn allow(&self, site: &str) -> bool {
        let guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(st) = guard.get(site) else {
            return true;
        };
        match st.opened_at {
            None => true,
            Some(at) => at.elapsed() >= self.cooldown,
        }
    }

    pub fn success(&self, site: &str) {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let st = guard.entry(site.to_string()).or_default();
        st.failures = 0;
        st.opened_at = None;
    }

    pub fn failure(&self, site: &str) {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let st = guard.entry(site.to_string()).or_default();
        st.failures += 1;
        if st.failures >= self.threshold {
            if st.opened_at.is_none() {
                st.trips += 1;
            }
            st.opened_at = Some(Instant::now());
        }
    }

    pub fn snapshot(&self) -> HashMap<String, BreakerEntry> {
        let guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        guard
            .iter()
            .map(|(k, v)| {
                let open = v
                    .opened_at
                    .map(|at| at.elapsed() < self.cooldown)
                    .unwrap_or(false);
                (
                    k.clone(),
                    BreakerEntry {
                        failures: v.failures,
                        open,
                        trips: v.trips,
                    },
                )
            })
            .collect()
    }
}

static OPEN_WEB_DOWN_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

fn open_web_terms(query: &str) -> Vec<String> {
    let gen_set = generic_set();
    let mut out = Vec::new();
    for t in query_terms(query, false) {
        if t.len() >= 4 && !gen_set.contains(t.as_str()) && !out.contains(&t) {
            out.push(t);
            if out.len() >= 2 {
                break;
            }
        }
    }
    out
}

async fn run_open_web(
    query: &str,
    reg: &Registry,
    plan: &Plan,
    fetcher: &Fetcher,
) -> Vec<Candidate> {
    if plan.fallback.as_deref() != Some("general") {
        return Vec::new();
    }
    let Some(ref cfg) = reg.open_web else {
        return Vec::new();
    };
    if !cfg.enabled {
        return Vec::new();
    }
    if let Some(until) = *OPEN_WEB_DOWN_UNTIL
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        && Instant::now() < until
    {
        return Vec::new();
    }
    let terms = open_web_terms(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let index_url = cfg
        .index
        .as_deref()
        .unwrap_or("https://index.commoncrawl.org/CC-MAIN-2026-43-index");
    let domains: Vec<String> = if cfg.domains.is_empty() {
        vec![
            "en.wikipedia.org".into(),
            "arxiv.org".into(),
            "github.com".into(),
        ]
    } else {
        cfg.domains
            .iter()
            .take(cfg.max_domains.unwrap_or(3))
            .cloned()
            .collect()
    };

    let work = async {
        let mut found = Vec::new();
        let mut seen = HashSet::new();
        for domain in domains {
            for term in &terms {
                let pattern = format!("{domain}/*{term}*");
                let u = format!(
                    "{index_url}?url={}&output=json&limit=20",
                    q_encode(&pattern)
                );
                let res = fetcher
                    .get(
                        &u,
                        GetOpts {
                            timeout_s: 6.0,
                            ..Default::default()
                        },
                    )
                    .await;
                if !res.ok() {
                    if matches!(res.status, Some(502..=504) | None) {
                        *OPEN_WEB_DOWN_UNTIL
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner) =
                            Some(Instant::now() + Duration::from_secs_f64(OPEN_WEB_COOLDOWN_S));
                    }
                    return found;
                }
                for line in res
                    .text
                    .lines()
                    .map(str::trim)
                    .filter(|l| l.starts_with('{'))
                {
                    let Ok(rec) = serde_json::from_str::<Value>(line) else {
                        continue;
                    };
                    let target = rec["url"].as_str().unwrap_or("").trim().to_string();
                    if !target.starts_with("http") || !seen.insert(target.clone()) {
                        continue;
                    }
                    found.push(Candidate {
                        url: target.clone(),
                        title: unslug(&target),
                        snippet: format!("{domain} {}", fold(term)),
                        published: rec["timestamp"].as_str().map(str::to_string),
                        source: "openweb".into(),
                        method: "cdx".into(),
                        score: 0.0,
                        body: String::new(),
                    });
                }
            }
        }
        found
    };

    let found = match tokio::time::timeout(Duration::from_secs_f64(OPEN_WEB_BUDGET_S), work).await {
        Ok(v) => v,
        Err(_) => {
            *OPEN_WEB_DOWN_UNTIL
                .lock()
                .unwrap_or_else(PoisonError::into_inner) =
                Some(Instant::now() + Duration::from_secs_f64(OPEN_WEB_COOLDOWN_S));
            return Vec::new();
        }
    };
    let mut ranked = score_candidates(query, found, 20);
    for c in &mut ranked {
        c.score = (c.score * 0.8 * 10000.0).round() / 10000.0;
    }
    ranked
}

pub struct DiscoveryResult {
    pub plan: Plan,
    pub open_web_used: bool,
    pub candidates: Vec<Candidate>,
    pub sources_with_results: usize,
}

fn indexable(site: &Site, index: Option<&SiteIndex>) -> bool {
    let Some(idx) = index else { return false };
    if !site
        .methods
        .iter()
        .any(|m| m.kind == "sitemap" || m.kind == "rss")
    {
        return false;
    }
    idx.site_pages(&site.id) > 0
}

fn is_recall_only_site(site: &Site) -> bool {
    site.id == "cpsc"
        || site.id == "guvensizurun"
        || (site.topics.len() == 1 && site.topics[0] == "recalls")
}

#[allow(clippy::too_many_arguments)]
pub async fn discover(
    query: &str,
    reg: &Registry,
    tranco: &HashMap<String, u32>,
    fetcher: &std::sync::Arc<Fetcher>,
    index: Option<&std::sync::Arc<SiteIndex>>,
    breaker: &std::sync::Arc<CircuitBreaker>,
    sitemap_mem: &std::sync::Arc<SitemapMem>,
    trust: &HashMap<String, f64>,
    max_sites: usize,
    max_candidates: usize,
) -> DiscoveryResult {
    let mut plan = plan_sites(query, reg, tranco, max_sites);
    let recall_query = plan.topics.iter().any(|t| t == "recalls") || is_recall_query(query);
    let idx_ref = index.map(|a| a.as_ref());

    // 1. Index-first domain-agnostic retrieval across all eligible indexed sites.
    let mut outcomes: Vec<AdapterOutcome> = Vec::new();
    let indexed_sites: Vec<&Site> = reg
        .sites
        .iter()
        .filter(|s| indexable(s, idx_ref) && (!is_recall_only_site(s) || recall_query))
        .collect();

    if let Some(idx) = index
        && !indexed_sites.is_empty()
    {
        let mut all_ids = Vec::with_capacity(indexed_sites.len());
        let mut ages = HashMap::new();
        let mut max_per_site = 30usize;
        for s in &indexed_sites {
            if let Some(d) = s.index_max_age_days {
                ages.insert(s.id.clone(), d);
            }
            let mr = s
                .methods
                .iter()
                .filter(|m| m.kind == "sitemap" || m.kind == "rss")
                .filter_map(|m| m.max_results)
                .max()
                .unwrap_or(30);
            max_per_site = max_per_site.max(mr);
            all_ids.push(s.id.clone());
        }
        let idx_c = idx.clone();
        let q_c = query.to_string();
        let by_site = tokio::task::spawn_blocking(move || {
            idx_c.query_sites(&all_ids, &q_c, max_per_site, 250, &ages)
        })
        .await
        .unwrap_or_default();

        for (sid, cands) in by_site {
            let st = if cands.is_empty() { "no-results" } else { "ok" };
            outcomes.push(AdapterOutcome {
                site: sid,
                method: "index".into(),
                status: st.into(),
                candidates: cands,
                fetched: 0,
                error: None,
            });
        }
    }

    let index_hits: usize = outcomes.iter().map(|o| o.candidates.len()).sum();

    // 2. Plan & run unindexed/live adapters independently so indexed sites never crowd out live APIs (e.g. CPSC recalls).
    let live_pool: Vec<Site> = reg
        .sites
        .iter()
        .filter(|s| !indexable(s, idx_ref))
        .cloned()
        .collect();
    let mut live_sites: Vec<Site> = if live_pool.is_empty() {
        Vec::new()
    } else {
        let live_reg = Registry {
            version: reg.version,
            sites: live_pool.clone(),
            open_web: None,
            skipped: vec![],
        };
        let lp = plan_sites(query, &live_reg, tranco, max_sites);
        lp.selected
            .into_iter()
            .filter(|s| {
                if s.topics.iter().any(|t| t == "general") {
                    plan.fallback.as_deref() == Some("general") || index_hits < 3
                } else {
                    true
                }
            })
            .collect()
    };
    if index_hits < 3 {
        for s in live_pool {
            if s.topics.iter().any(|t| t == "general")
                && !live_sites.iter().any(|x| x.id == s.id)
                && (s.language == plan.language || s.language == "en")
            {
                live_sites.push(s);
            }
        }
    }

    let mut set = JoinSet::new();
    for site in live_sites {
        let q = query.to_string();
        let f = fetcher.clone();
        let br = breaker.clone();
        let sm = sitemap_mem.clone();
        set.spawn(async move {
            if !br.allow(&site.id) {
                return vec![AdapterOutcome {
                    site: site.id,
                    method: "-".into(),
                    status: "circuit-open".into(),
                    candidates: Vec::new(),
                    fetched: 0,
                    error: Some("skipped: circuit open".into()),
                }];
            }
            let timeout_s = site.timeout_s.unwrap_or(SITE_TIMEOUT_S);
            match tokio::time::timeout(
                Duration::from_secs_f64(timeout_s),
                run_site(&site, &q, &f, &sm),
            )
            .await
            {
                Ok(outcomes) => {
                    let failed = !outcomes.iter().any(|o| !o.candidates.is_empty())
                        && outcomes.iter().any(|o| o.status == "error");
                    if failed {
                        br.failure(&site.id);
                    } else if !outcomes.is_empty() {
                        br.success(&site.id);
                    }
                    outcomes
                }
                Err(_) => {
                    br.failure(&site.id);
                    vec![AdapterOutcome {
                        site: site.id,
                        method: "?".into(),
                        status: "error".into(),
                        candidates: Vec::new(),
                        fetched: 0,
                        error: Some("timeout".into()),
                    }]
                }
            }
        });
    }

    while let Some(res) = set.join_next().await {
        if let Ok(list) = res {
            outcomes.extend(list);
        }
    }

    let mut merged: HashMap<String, Candidate> = HashMap::new();
    let collect = |list: &[AdapterOutcome], map: &mut HashMap<String, Candidate>| {
        let log_verbose = std::env::var("KRIKO_LOG")
            .map(|v| matches!(v.to_ascii_uppercase().as_str(), "INFO" | "DEBUG"))
            .unwrap_or(false);
        for out in list {
            if log_verbose && out.status == "error" {
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "site": out.site,
                        "method": out.method,
                        "status": out.status,
                        "fetched": out.fetched,
                        "error": out.error,
                    })
                );
            }
            for mut cand in out.candidates.clone() {
                let base = base_review_url(&cand.url);
                if base != cand.url {
                    cand.url = base.clone();
                    cand.title = unslug(&base);
                    cand.snippet = format!("{} {}", cand.source, cand.title);
                }
                let key = canonical_url(&cand.url);
                let replace = map
                    .get(&key)
                    .map(|prev| cand.score > prev.score)
                    .unwrap_or(true);
                if replace {
                    map.insert(key, cand);
                }
            }
        }
    };
    collect(&outcomes, &mut merged);

    let mut open_web_used = false;
    if merged.is_empty() {
        let extra = run_open_web(query, reg, &plan, fetcher).await;
        if !extra.is_empty() {
            open_web_used = true;
            let o = AdapterOutcome {
                site: "openweb".into(),
                method: "cdx".into(),
                status: "ok".into(),
                candidates: extra,
                fetched: 0,
                error: None,
            };
            collect(std::slice::from_ref(&o), &mut merged);
        }
    }

    let site_langs: HashMap<&str, &str> = reg
        .sites
        .iter()
        .map(|s| (s.id.as_str(), s.language.as_str()))
        .collect();
    let recall_ids: HashSet<&str> = reg
        .sites
        .iter()
        .filter(|s| is_recall_only_site(s))
        .map(|s| s.id.as_str())
        .collect();
    let planned_bonus: HashMap<String, f64> = plan
        .selected
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.clone(), (0.20 - i as f64 * 0.03).max(0.05)))
        .collect();

    let mut candidates: Vec<Candidate> = merged.into_values().collect();
    candidates = collapse_paginated(candidates);
    candidates = score_candidates(query, candidates, max_candidates);

    // Apply language affinity, planner alignment, recall authority, and probabilistic source trust AFTER score_candidates
    for c in &mut candidates {
        if let Some(&sl) = site_langs.get(c.source.as_str())
            && sl == plan.language
        {
            c.score += if plan.language == "en" { 0.18 } else { 0.06 };
        }
        if let Some(&b) = planned_bonus.get(&c.source) {
            c.score += b;
        }
        if recall_query && recall_ids.contains(c.source.as_str()) {
            c.score += 0.25;
        }
        let dom = base_domain(&c.url);
        if let Some(&p_trust) = trust.get(&dom) {
            c.score += 0.06 * (p_trust - 0.5);
        }
        c.score = (c.score * 10000.0).round() / 10000.0;
    }
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));

    candidates = enforce_model_terms(query, candidates);
    candidates = enforce_brand_terms(query, candidates);
    candidates = enforce_product_terms(query, candidates);
    candidates = enforce_intent_terms(query, candidates);
    candidates = latest_year(query, candidates, &plan.topics);

    let sources_with_results: HashSet<String> =
        candidates.iter().map(|c| c.source.clone()).collect();
    let mut plan_sites_ordered: Vec<String> = Vec::new();
    for c in &candidates {
        if !plan_sites_ordered.contains(&c.source) {
            plan_sites_ordered.push(c.source.clone());
        }
    }
    for s in plan.sites {
        if !plan_sites_ordered.contains(&s) {
            plan_sites_ordered.push(s);
        }
    }
    plan_sites_ordered.truncate(max_sites.max(6));
    plan.sites = plan_sites_ordered;

    DiscoveryResult {
        plan,
        open_web_used,
        sources_with_results: sources_with_results.len(),
        candidates,
    }
}

pub fn to_rank_inputs(candidates: &[Candidate]) -> Vec<RankInput> {
    candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let published = if matches!(c.method.as_str(), "api" | "rss" | "search") {
                c.published.clone().unwrap_or_else(|| long_ago(180))
            } else {
                long_ago(90)
            };
            RankInput {
                id: format!("c{i:04}"),
                domain: base_domain(&c.url),
                title: ranker_text(&c.title),
                snippet: ranker_text(&c.snippet),
                published,
                discovery: c.score,
            }
        })
        .collect()
}

pub fn health_query_for(site_id: &str) -> &'static str {
    match site_id {
        "gsmarena" | "donanimhaber" | "webtekno" | "chip" => "iphone 15",
        "rtings" => "lg c1",
        "cpsc" => "anker",
        "ifixit" => "iphone screen replacement",
        "vaillant" => "f28 fault code",
        "vaillanttr" | "baymak" => "kombi arıza",
        "consumeraffairs" => "samsung refrigerator",
        "whathifi" => "lg oled",
        "macrumors" => "iphone battery",
        "tomshardware" => "rtx 4070",
        "caranddriver" => "honda cr-v",
        "guvensizurun" => "güvensiz ürün",
        "boschhome" => "bulaşık makinesi",
        "eca" => "kombi basıncı",
        "donanimarsivi" => "iphone 17",
        "shiftdelete" => "iphone inceleme",
        "tamindir" => "iphone",
        "wikipedia" | "wikipedia-tr" | "crossref" | "mathstackexchange" | "stackoverflow" => {
            "kolmogorov"
        }
        _ => "review",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sitemap_and_rss_and_scores() {
        let idx_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
            <sitemapindex><sitemap><loc>https://example.com/sitemap-guides.xml</loc></sitemap></sitemapindex>"#;
        let (k, entries) = parse_xml(idx_xml);
        assert_eq!(k, "sitemapindex");
        assert_eq!(entries.len(), 1);

        let rss_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
            <rss><channel><item><title>Detergents</title><link>https://example.gov/notice/20983</link>
            <description>çamaşır suyu güvensiz</description></item></channel></rss>"#;
        let (rk, rentries) = parse_xml(rss_xml);
        assert_eq!(rk, "rss");
        assert_eq!(rentries.len(), 1);

        let cands = vec![
            Candidate {
                url: "https://www.consumeraffairs.com/homeowners/samsung_refrigerator.html".into(),
                title: "homeowners samsung refrigerator".into(),
                snippet: "ConsumerAffairs homeowners samsung refrigerator".into(),
                published: None,
                source: "consumeraffairs".into(),
                method: "sitemap".into(),
                score: 0.0,
                body: String::new(),
            },
            Candidate {
                url: "https://example.com/samsung-phone".into(),
                title: "samsung phone".into(),
                snippet: "samsung phone".into(),
                published: None,
                source: "ex".into(),
                method: "sitemap".into(),
                score: 0.0,
                body: String::new(),
            },
        ];
        let ranked = score_candidates("samsung buzdolabı sorunu", cands, 5);
        assert_eq!(ranked.len(), 1);
        assert!(ranked[0].url.contains("samsung_refrigerator"));
    }

    #[test]
    fn circuit_breaker_opens_and_resets() {
        let br = CircuitBreaker::new(3, 300.0);
        assert!(br.allow("rtings"));
        br.failure("rtings");
        br.failure("rtings");
        assert!(br.allow("rtings"));
        br.failure("rtings");
        assert!(!br.allow("rtings"));
        assert!(br.allow("gsmarena"));
        br.success("rtings");
        assert!(br.allow("rtings"));
    }

    #[test]
    fn model_brand_product_and_intent_filters() {
        let mk = |u: &str, t: &str| Candidate {
            url: u.into(),
            title: t.into(),
            snippet: t.into(),
            published: None,
            source: "ex".into(),
            method: "sitemap".into(),
            score: 1.0,
            body: String::new(),
        };
        let cands = vec![
            mk(
                "https://www.rtings.com/tv/reviews/lg/c1-oled",
                "tv reviews lg c1 oled",
            ),
            mk(
                "https://www.rtings.com/tv/learn/burn-in",
                "tv learn oled burn in",
            ),
            mk(
                "https://www.rtings.com/running-shoes/c1",
                "running shoes c1",
            ),
        ];
        let model = enforce_model_terms("lg c1 burn in", cands);
        assert_eq!(model.len(), 1);
        let brand = enforce_brand_terms("lg c1 burn in", model);
        assert_eq!(brand[0].url, "https://www.rtings.com/tv/reviews/lg/c1-oled");

        let prod = enforce_product_terms(
            "whirlpool dishwasher not draining",
            vec![
                mk(
                    "https://example.com/whirlpool-dishwashers",
                    "Whirlpool dishwashers",
                ),
                mk(
                    "https://example.com/whirlpool-fridge",
                    "Whirlpool refrigerator",
                ),
            ],
        );
        assert_eq!(prod.len(), 1);
        assert!(prod[0].url.contains("dishwashers"));

        let intent = enforce_intent_terms(
            "lg washing machine review",
            vec![
                mk(
                    "https://example.com/lg-washing-machine-repair",
                    "LG washing machine repair",
                ),
                mk(
                    "https://example.com/lg-washing-machine-review",
                    "LG washing machine review",
                ),
            ],
        );
        assert_eq!(intent.len(), 1);
        assert!(intent[0].url.contains("review"));
    }

    #[tokio::test]
    async fn json_search_and_html_search_adapters() {
        use crate::http::{RawResponse, Transport};
        use crate::planner::Method;
        use std::future::Future;
        use std::pin::Pin;

        struct Stub(HashMap<String, String>);
        impl Transport for Stub {
            fn get<'a>(
                &'a self,
                url: &'a str,
                _h: &'a HashMap<String, String>,
                _t: Duration,
            ) -> Pin<Box<dyn Future<Output = Result<RawResponse, String>> + Send + 'a>>
            {
                Box::pin(async move {
                    let body = self.0.get(url).cloned().unwrap_or_default();
                    Ok(RawResponse {
                        status: if body.is_empty() { 404 } else { 200 },
                        body: body.into_bytes(),
                        headers: HashMap::new(),
                        final_url: url.to_string(),
                    })
                })
            }
        }

        let dir = std::env::temp_dir().join(format!("magnet-disc-{}", rand::random::<u64>()));
        let mut map = HashMap::new();
        map.insert(
            "https://api.example.test/robots.txt".into(),
            "User-agent: *\nAllow: /\n".into(),
        );
        map.insert(
            "https://api.example.test/search?q=kolmogorov+complexity&limit=10".into(),
            r#"{"pages":[{"key":"Kolmogorov_complexity","title":"Kolmogorov complexity","excerpt":"<b>Kolmogorov</b> complexity measure"}]}"#.into(),
        );
        map.insert(
            "https://example.com/robots.txt".into(),
            "User-agent: *\nAllow: /\n".into(),
        );
        map.insert(
            "https://example.com/search?q=anker".into(),
            r#"<html><body><a href="/recalls/2025/anker-power-banks">Anker Power Banks Recall</a></body></html>"#.into(),
        );
        let fetcher =
            Fetcher::with_transport(&dir.join("f.sqlite"), Box::new(Stub(map)), 2, 0.0).unwrap();

        let wiki = Site {
            id: "wikipedia".into(),
            name: "Wikipedia".into(),
            domain: "en.wikipedia.org".into(),
            topics: vec!["general".into()],
            language: "en".into(),
            priority: None,
            timeout_s: None,
            index_max_age_days: None,
            index_max_children: None,
            methods: vec![Method {
                kind: "json_search".into(),
                url: Some("https://api.example.test/search?q={query}&limit=10".into()),
                urls: vec![],
                include_children: vec![],
                exclude_children: vec![],
                max_sitemaps: None,
                max_urls: None,
                max_results: Some(10),
                probes: Some(1),
                link_selector: None,
                base_url: None,
                items_path: Some("pages".into()),
                title: Some("title".into()),
                snippet: Some("excerpt".into()),
                url_field: Some("key".into()),
                url_template: Some("https://en.wikipedia.org/wiki/{value}".into()),
                body_field: None,
                date: None,
                timeout: None,
                templates: vec![],
                max_query_terms: None,
            }],
        };
        let out = run_json_search(&wiki, &wiki.methods[0], "kolmogorov complexity", &fetcher).await;
        assert_eq!(out.status, "ok");
        assert_eq!(
            out.candidates[0].url,
            "https://en.wikipedia.org/wiki/Kolmogorov_complexity"
        );

        let ex = Site {
            id: "ex".into(),
            name: "Ex".into(),
            domain: "example.com".into(),
            topics: vec!["recalls".into()],
            language: "en".into(),
            priority: None,
            timeout_s: None,
            index_max_age_days: None,
            index_max_children: None,
            methods: vec![Method {
                kind: "search".into(),
                url: Some("https://example.com/search?q={query}".into()),
                urls: vec![],
                include_children: vec![],
                exclude_children: vec![],
                max_sitemaps: None,
                max_urls: None,
                max_results: Some(10),
                probes: None,
                link_selector: Some("a[href^=\"/recalls/\"]".into()),
                base_url: None,
                items_path: None,
                title: None,
                snippet: None,
                url_field: None,
                url_template: None,
                body_field: None,
                date: None,
                timeout: None,
                templates: vec![],
                max_query_terms: None,
            }],
        };
        let out2 = run_search(&ex, &ex.methods[0], "anker", &fetcher).await;
        assert_eq!(out2.status, "ok");
        assert_eq!(
            out2.candidates[0].url,
            "https://example.com/recalls/2025/anker-power-banks"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn index_first_finds_arbitrary_topic_without_topic_terms() {
        let dir = std::env::temp_dir().join(format!("magnet-idxfirst-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let idx = std::sync::Arc::new(SiteIndex::open(&dir.join("idx.sqlite")).unwrap());
        let rows = vec![crate::index::IndexRow {
            url: "https://plato.stanford.edu/entries/godel-incompleteness/".into(),
            title: "Godel Incompleteness Theorems".into(),
            snippet: "Formal arithmetic cannot prove its own consistency.".into(),
            lastmod: Some("2026-01-15T00:00:00Z".into()),
            method: "sitemap".into(),
            text: "godel incompleteness theorems formal arithmetic consistency".into(),
        }];
        idx.replace_site("sep", rows, true).unwrap();

        let reg = Registry {
            version: 1,
            sites: vec![Site {
                id: "sep".into(),
                name: "Stanford Encyclopedia".into(),
                domain: "plato.stanford.edu".into(),
                topics: vec!["philosophy".into()],
                language: "en".into(),
                priority: None,
                timeout_s: None,
                index_max_age_days: None,
                index_max_children: None,
                methods: vec![Method {
                    kind: "sitemap".into(),
                    url: Some("https://plato.stanford.edu/sitemap.xml".into()),
                    urls: vec![],
                    include_children: vec![],
                    exclude_children: vec![],
                    max_sitemaps: None,
                    max_urls: None,
                    max_results: Some(10),
                    probes: None,
                    link_selector: None,
                    base_url: None,
                    items_path: None,
                    title: None,
                    snippet: None,
                    url_field: None,
                    url_template: None,
                    body_field: None,
                    date: None,
                    timeout: None,
                    templates: vec![],
                    max_query_terms: None,
                }],
            }],
            open_web: None,
            skipped: vec![],
        };
        let tr = HashMap::new();
        let fetcher = std::sync::Arc::new(Fetcher::open(&dir.join("fetch.sqlite")).unwrap());
        let br = std::sync::Arc::new(CircuitBreaker::new(3, 60.0));
        let sm = std::sync::Arc::new(Mutex::new(HashMap::new()));
        let trust = HashMap::from([("plato.stanford.edu".to_string(), 0.96)]);

        let disc = discover(
            "godel incompleteness consistency",
            &reg,
            &tr,
            &fetcher,
            Some(&idx),
            &br,
            &sm,
            &trust,
            6,
            40,
        )
        .await;
        assert_eq!(disc.candidates.len(), 1);
        assert_eq!(
            disc.candidates[0].url,
            "https://plato.stanford.edu/entries/godel-incompleteness/"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
