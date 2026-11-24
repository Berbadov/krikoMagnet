use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use regex::Regex;
use rusqlite::Connection;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use url::Url;

use crate::discover::base_domain;
use crate::extract::{SOFT_MIN_CHARS, classify_fetch, extract_text};
use crate::http::{Fetcher, GetOpts, decode_body};
use crate::planner::Registry;
use crate::text::tokenize;

const AFFILIATE_MARKERS: &[&str] = &[
    "/go/",
    "/out/",
    "/redirect",
    "amzn.to",
    "tag=",
    "aff=",
    "affiliate",
    "bit.ly",
    "geni.us",
    "awin1.com",
    "anrdoezrs.net",
    "tkqlhce.com",
    "skimlinks",
    "linksynergy",
];

#[derive(Clone, Debug, Serialize)]
pub struct PageShape {
    pub url: String,
    pub host: String,
    pub verdict: String,
    pub clean_chars: usize,
    pub visible_chars: usize,
    pub yield_ratio: f64,
    pub block_entropy: f64,
    pub structured_ratio: f64,
    pub section_cv: f64,
    pub sentence_burstiness: f64,
    pub zlib_ratio: f64,
    pub mattr: f64,
    pub trigram_repeat: f64,
    pub specificity: f64,
    pub link_density: f64,
    pub affiliate_ratio: f64,
    pub heading_count: usize,
    pub list_count: usize,
    #[serde(skip_serializing)]
    pub shingles: HashSet<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SiteShape {
    pub host: String,
    pub pages_sampled: usize,
    pub access_rate: f64,
    pub median_yield: f64,
    pub median_block_entropy: f64,
    pub median_structured_ratio: f64,
    pub median_section_cv: f64,
    pub median_burstiness: f64,
    pub median_zlib_ratio: f64,
    pub median_mattr: f64,
    pub median_trigram_repeat: f64,
    pub median_specificity: f64,
    pub median_link_density: f64,
    pub median_affiliate_ratio: f64,
    pub cross_page_dup: f64,
    pub skeleton_cv: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeatureStats {
    pub mean: f64,
    pub std: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GoldenBaseline {
    pub sites_calibrated: usize,
    pub pages_calibrated: usize,
    pub yield_ratio: FeatureStats,
    pub block_entropy: FeatureStats,
    pub section_cv: FeatureStats,
    pub burstiness: FeatureStats,
    pub zlib_ratio: FeatureStats,
    pub mattr: FeatureStats,
    pub trigram_repeat: FeatureStats,
    pub specificity: FeatureStats,
    pub link_density: FeatureStats,
    pub affiliate_ratio: FeatureStats,
    pub cross_page_dup: FeatureStats,
    pub skeleton_cv: FeatureStats,
}

impl Default for GoldenBaseline {
    fn default() -> Self {
        Self {
            sites_calibrated: 0,
            pages_calibrated: 0,
            yield_ratio: FeatureStats { mean: 0.55, std: 0.25 },
            block_entropy: FeatureStats { mean: 0.62, std: 0.18 },
            section_cv: FeatureStats { mean: 0.65, std: 0.30 },
            burstiness: FeatureStats { mean: 0.78, std: 0.28 },
            zlib_ratio: FeatureStats { mean: 0.42, std: 0.08 },
            mattr: FeatureStats { mean: 0.80, std: 0.07 },
            trigram_repeat: FeatureStats { mean: 0.04, std: 0.04 },
            specificity: FeatureStats { mean: 0.10, std: 0.06 },
            link_density: FeatureStats { mean: 0.15, std: 0.15 },
            affiliate_ratio: FeatureStats { mean: 0.01, std: 0.03 },
            cross_page_dup: FeatureStats { mean: 0.03, std: 0.05 },
            skeleton_cv: FeatureStats { mean: 0.45, std: 0.28 },
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PosteriorEstimate {
    pub host: String,
    pub pages_sampled: usize,
    pub p_golden: f64,
    pub p_low: f64,
    pub p_high: f64,
    pub log_odds: f64,
    pub decision: String,
    pub contributions: HashMap<String, f64>,
    pub shape: SiteShape,
}

pub fn baseline_default_path() -> PathBuf {
    PathBuf::from("data").join("source_baseline.json")
}

pub fn load_baseline(path: &Path) -> GoldenBaseline {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

fn hash_shingle(words: &[&str]) -> u64 {
    let mut h = Sha256::new();
    for w in words {
        h.update(w.as_bytes());
        h.update(b"\x00");
    }
    let out = h.finalize();
    u64::from_le_bytes(out[..8].try_into().unwrap())
}

fn prose_from_markdown(md: &str) -> String {
    static IMG_RE: OnceLock<Regex> = OnceLock::new();
    static LINK_RE: OnceLock<Regex> = OnceLock::new();
    static URL_RE: OnceLock<Regex> = OnceLock::new();
    let img_re = IMG_RE.get_or_init(|| Regex::new(r"!\[[^\]]*\]\([^)]+\)").unwrap());
    let link_re = LINK_RE.get_or_init(|| Regex::new(r"\[([^\]]+)\]\([^)]+\)").unwrap());
    let url_re = URL_RE.get_or_init(|| Regex::new(r"https?://\S+").unwrap());
    let no_img = img_re.replace_all(md, " ");
    let no_links = link_re.replace_all(&no_img, "$1");
    url_re.replace_all(&no_links, " ").into_owned()
}

fn zlib_compress_ratio(text: &str) -> f64 {
    if text.is_empty() {
        return 1.0;
    }
    let raw = text.as_bytes();
    let chunk_size = 2048usize;
    if raw.len() <= chunk_size {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        if enc.write_all(raw).is_err() {
            return 1.0;
        }
        return enc.finish().map(|c| c.len() as f64 / raw.len() as f64).unwrap_or(1.0);
    }
    let mut sum = 0.0;
    let mut n = 0usize;
    for chunk in raw.chunks(chunk_size) {
        if chunk.len() < 256 {
            continue;
        }
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        if enc.write_all(chunk).is_ok()
            && let Ok(comp) = enc.finish()
        {
            sum += comp.len() as f64 / chunk.len() as f64;
            n += 1;
        }
    }
    if n > 0 { sum / n as f64 } else { 1.0 }
}

fn mattr_score(tokens: &[String], win: usize) -> f64 {
    if tokens.is_empty() {
        return 0.0;
    }
    if tokens.len() < win {
        let uniq: HashSet<&str> = tokens.iter().map(String::as_str).collect();
        return uniq.len() as f64 / tokens.len() as f64;
    }
    let mut sum = 0.0;
    let mut count = 0usize;
    let step = 10;
    let mut i = 0usize;
    while i + win <= tokens.len() {
        let uniq: HashSet<&str> = tokens[i..i + win].iter().map(String::as_str).collect();
        sum += uniq.len() as f64 / win as f64;
        count += 1;
        i += step;
    }
    sum / count.max(1) as f64
}

fn trigram_repeat_ratio(tokens: &[String]) -> f64 {
    if tokens.len() < 6 {
        return 0.0;
    }
    let mut counts: HashMap<(&str, &str, &str), usize> = HashMap::new();
    let total = tokens.len() - 2;
    for w in tokens.windows(3) {
        *counts
            .entry((w[0].as_str(), w[1].as_str(), w[2].as_str()))
            .or_insert(0) += 1;
    }
    let repeated: usize = counts.values().filter(|&&c| c > 1).map(|&c| c - 1).sum();
    repeated as f64 / total as f64
}

fn sentence_burstiness(clean_md: &str) -> f64 {
    static SENT_RE: OnceLock<Regex> = OnceLock::new();
    let sent_re = SENT_RE.get_or_init(|| Regex::new(r"[.!?\n]+").unwrap());
    let lens: Vec<f64> = sent_re
        .split(clean_md)
        .map(|s| s.split_whitespace().count())
        .filter(|&n| n >= 3)
        .map(|n| n as f64)
        .collect();
    if lens.len() < 4 {
        return 0.0;
    }
    let mu = lens.iter().sum::<f64>() / lens.len() as f64;
    if mu <= 0.1 {
        return 0.0;
    }
    let var = lens.iter().map(|&x| (x - mu).powi(2)).sum::<f64>() / lens.len() as f64;
    var.sqrt() / mu
}

fn section_length_cv(clean_md: &str) -> f64 {
    let mut sections = Vec::new();
    let mut cur = 0usize;
    for line in clean_md.lines() {
        let t = line.trim_start();
        if t.starts_with('#') {
            if cur > 0 {
                sections.push(cur as f64);
                cur = 0;
            }
        } else {
            cur += line.split_whitespace().count();
        }
    }
    if cur > 0 {
        sections.push(cur as f64);
    }
    if sections.len() < 3 {
        return 0.5; // neutral when page has <=2 sections (e.g., short guide/encyclopedia)
    }
    let mu = sections.iter().sum::<f64>() / sections.len() as f64;
    if mu <= 1.0 {
        return 0.0;
    }
    let var = sections.iter().map(|&x| (x - mu).powi(2)).sum::<f64>() / sections.len() as f64;
    var.sqrt() / mu
}

pub fn measure_page(url: &str, html: &str, status: Option<u16>) -> PageShape {
    let host = Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_else(|| base_domain(url));
    let clean_md = extract_text(html, Some(url), SOFT_MIN_CHARS);
    let clean_chars = clean_md.chars().count();
    let status_tag = status.map(|s| s.to_string()).unwrap_or_else(|| "ERR".into());
    let verdict = classify_fetch(status, &status_tag, clean_chars, &clean_md).to_string();

    let doc = Html::parse_document(html);
    let root_sel = Selector::parse("article, main, body").unwrap();
    let p_sel = Selector::parse("p").unwrap();
    let h_sel = Selector::parse("h1, h2, h3, h4").unwrap();
    let li_sel = Selector::parse("li").unwrap();
    let tbl_sel = Selector::parse("table").unwrap();
    let code_sel = Selector::parse("pre, code").unwrap();
    let quote_sel = Selector::parse("blockquote").unwrap();
    let a_sel = Selector::parse("a[href]").unwrap();

    let mut visible_chars = 0usize;
    let mut heading_count = 0usize;
    let mut list_count = 0usize;
    let mut link_chars = 0usize;
    let mut aff_links = 0usize;
    let mut block_counts = [0usize; 6]; // p, h, li, table, code, quote

    if let Some(root) = doc.select(&root_sel).next() {
        for node in root.descendants() {
            if let Some(txt) = node.value().as_text() {
                let inside_skip = node.ancestors().any(|a| {
                    matches!(
                        a.value().as_element().map(|e| e.name()),
                        Some("script" | "style" | "noscript" | "svg" | "nav" | "footer")
                    )
                });
                if !inside_skip {
                    visible_chars += txt.split_whitespace().map(|w| w.len() + 1).sum::<usize>();
                }
            }
        }
        block_counts[0] = root.select(&p_sel).count();
        heading_count = root.select(&h_sel).count();
        block_counts[1] = heading_count;
        list_count = root.select(&li_sel).count();
        block_counts[2] = list_count;
        block_counts[3] = root.select(&tbl_sel).count();
        block_counts[4] = root.select(&code_sel).count();
        block_counts[5] = root.select(&quote_sel).count();

        let base_dom = base_domain(url);
        for a in root.select(&a_sel) {
            let inside_nav = a.ancestors().any(|anc| {
                matches!(
                    anc.value().as_element().map(|e| e.name()),
                    Some("nav" | "footer" | "header")
                )
            });
            if inside_nav {
                continue;
            }
            let txt_len: usize = a.text().map(|t| t.trim().len()).sum();
            link_chars += txt_len;
            if let Some(href) = a.value().attr("href") {
                let low = href.to_ascii_lowercase();
                if low.starts_with("http")
                    && !low.contains(&base_dom)
                    && AFFILIATE_MARKERS.iter().any(|m| low.contains(m))
                {
                    aff_links += 1;
                }
            }
        }
    }

    let total_blocks: usize = block_counts.iter().sum();
    let block_entropy = if total_blocks == 0 {
        0.0
    } else {
        let mut h = 0.0;
        let tot = total_blocks as f64;
        for &c in &block_counts {
            if c > 0 {
                let p = c as f64 / tot;
                h -= p * p.ln();
            }
        }
        h / (block_counts.len() as f64).ln()
    };
    let structured_ratio =
        (block_counts[3] + block_counts[4] + block_counts[5]) as f64 / total_blocks.max(1) as f64;
    let yield_ratio = (clean_chars as f64 / visible_chars.max(1) as f64).min(1.5);
    let link_density = (link_chars as f64 / visible_chars.max(1) as f64).min(1.0);
    let kchars = (clean_chars.max(500) as f64) / 1000.0;
    let affiliate_ratio = (aff_links as f64 / (kchars * 4.0)).min(1.0);

    let prose = prose_from_markdown(&clean_md);
    let tokens = tokenize(&prose);
    let specific_toks = tokens
        .iter()
        .filter(|t| t.bytes().any(|b| b.is_ascii_digit()))
        .count();
    let specificity = specific_toks as f64 / tokens.len().max(1) as f64;

    let mut shingles = HashSet::new();
    let word_refs: Vec<&str> = tokens.iter().map(String::as_str).collect();
    for w in word_refs.windows(5).step_by(2).take(128) {
        shingles.insert(hash_shingle(w));
    }

    PageShape {
        url: url.to_string(),
        host,
        verdict,
        clean_chars,
        visible_chars,
        yield_ratio,
        block_entropy,
        structured_ratio,
        section_cv: section_length_cv(&clean_md),
        sentence_burstiness: sentence_burstiness(&prose),
        zlib_ratio: zlib_compress_ratio(&prose),
        mattr: mattr_score(&tokens, 50),
        trigram_repeat: trigram_repeat_ratio(&tokens),
        specificity,
        link_density,
        affiliate_ratio,
        heading_count,
        list_count,
        shingles,
    }
}

fn median_of(vals: &[f64]) -> f64 {
    if vals.is_empty() {
        return 0.0;
    }
    let mut s = vals.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let m = s.len() / 2;
    if s.len() % 2 == 1 {
        s[m]
    } else {
        (s[m - 1] + s[m]) / 2.0
    }
}

fn cv_of(vals: &[f64]) -> f64 {
    if vals.len() < 2 {
        return 0.45;
    }
    let mu = vals.iter().sum::<f64>() / vals.len() as f64;
    if mu <= 1e-6 {
        return 0.0;
    }
    let var = vals.iter().map(|&x| (x - mu).powi(2)).sum::<f64>() / vals.len() as f64;
    var.sqrt() / mu
}

pub fn aggregate_site(host: &str, pages: &[PageShape]) -> SiteShape {
    let n = pages.len().max(1) as f64;
    let access_rate = pages.iter().filter(|p| p.verdict == "success").count() as f64 / n;
    let collect = |f: fn(&PageShape) -> f64| -> Vec<f64> { pages.iter().map(f).collect() };

    let mut dup_sum = 0.0;
    let mut pairs = 0usize;
    for i in 0..pages.len() {
        for j in (i + 1)..pages.len() {
            let a = &pages[i].shingles;
            let b = &pages[j].shingles;
            if a.is_empty() || b.is_empty() {
                continue;
            }
            let inter = a.intersection(b).count();
            let union = a.len() + b.len() - inter;
            if union > 0 {
                dup_sum += inter as f64 / union as f64;
                pairs += 1;
            }
        }
    }
    let cross_page_dup = if pairs > 0 { dup_sum / pairs as f64 } else { 0.0 };

    let hd_cv = cv_of(&collect(|p| p.heading_count as f64));
    let li_cv = cv_of(&collect(|p| p.list_count as f64));
    let ch_cv = cv_of(&collect(|p| p.clean_chars as f64));
    let skeleton_cv = (hd_cv + li_cv + ch_cv) / 3.0;

    SiteShape {
        host: host.to_string(),
        pages_sampled: pages.len(),
        access_rate,
        median_yield: median_of(&collect(|p| p.yield_ratio)),
        median_block_entropy: median_of(&collect(|p| p.block_entropy)),
        median_structured_ratio: median_of(&collect(|p| p.structured_ratio)),
        median_section_cv: median_of(&collect(|p| p.section_cv)),
        median_burstiness: median_of(&collect(|p| p.sentence_burstiness)),
        median_zlib_ratio: median_of(&collect(|p| p.zlib_ratio)),
        median_mattr: median_of(&collect(|p| p.mattr)),
        median_trigram_repeat: median_of(&collect(|p| p.trigram_repeat)),
        median_specificity: median_of(&collect(|p| p.specificity)),
        median_link_density: median_of(&collect(|p| p.link_density)),
        median_affiliate_ratio: median_of(&collect(|p| p.affiliate_ratio)),
        cross_page_dup,
        skeleton_cv,
    }
}

/// One-sided log-odds where higher values are strictly suspicious (duplication, affiliates, repetition).
fn upper_penalty_llr(x: f64, st: &FeatureStats, weight: f64) -> f64 {
    let sd = st.std.max(0.02);
    let excess = ((x - (st.mean + 0.5 * sd)) / sd).max(0.0);
    if excess <= 0.0 {
        0.25 * weight
    } else {
        -weight * (excess * 0.85).tanh() * 2.5
    }
}

/// One-sided log-odds where higher values signal human/structural richness (burstiness, skeleton variance, section CV, yield, entropy).
fn lower_penalty_llr(x: f64, st: &FeatureStats, weight: f64) -> f64 {
    let sd = st.std.max(0.05);
    let deficit = (((st.mean - 0.5 * sd) - x) / sd).max(0.0);
    if deficit <= 0.0 {
        0.35 * weight
    } else {
        -weight * (deficit * 0.8).tanh() * 2.2
    }
}

pub fn estimate_site(
    shape: &SiteShape,
    baseline: &GoldenBaseline,
    prior_log_odds: f64,
) -> PosteriorEstimate {
    let mut c = HashMap::new();

    // Beta-Binomial log-odds for polite access & extractability (bounded so volume doesn't mask slop)
    let ok_n = shape.access_rate * shape.pages_sampled as f64;
    let fail_n = (1.0 - shape.access_rate) * shape.pages_sampled as f64;
    let access_llr = (ok_n.min(3.0) * 0.7 - fail_n * 2.5).clamp(-4.0, 2.0);
    c.insert("access".into(), access_llr);

    c.insert("burstiness".into(), lower_penalty_llr(shape.median_burstiness, &baseline.burstiness, 1.3));
    c.insert("section_cv".into(), lower_penalty_llr(shape.median_section_cv, &baseline.section_cv, 1.1));
    c.insert("skeleton_cv".into(), lower_penalty_llr(shape.skeleton_cv, &baseline.skeleton_cv, 1.3));
    c.insert("yield".into(), lower_penalty_llr(shape.median_yield, &baseline.yield_ratio, 0.8));
    c.insert("block_entropy".into(), lower_penalty_llr(shape.median_block_entropy, &baseline.block_entropy, 0.8));
    c.insert("zlib".into(), lower_penalty_llr(shape.median_zlib_ratio, &baseline.zlib_ratio, 0.8));
    c.insert("mattr".into(), lower_penalty_llr(shape.median_mattr, &baseline.mattr, 0.5));
    c.insert("specificity".into(), lower_penalty_llr(shape.median_specificity, &baseline.specificity, 0.7));
    c.insert("trigram_rep".into(), upper_penalty_llr(shape.median_trigram_repeat, &baseline.trigram_repeat, 0.9));
    c.insert("link_density".into(), upper_penalty_llr(shape.median_link_density, &baseline.link_density, 1.1));
    c.insert("affiliate".into(), upper_penalty_llr(shape.median_affiliate_ratio, &baseline.affiliate_ratio, 1.8));
    if shape.pages_sampled >= 2 {
        c.insert("cross_page_dup".into(), upper_penalty_llr(shape.cross_page_dup, &baseline.cross_page_dup, 1.2));
    }
    if shape.median_structured_ratio > 0.05 {
        c.insert("structured_bonus".into(), (shape.median_structured_ratio * 2.5).min(0.8));
    }

    let evidence: f64 = c.values().sum();
    let log_odds = prior_log_odds + evidence;
    let p_golden = sigmoid(log_odds);

    // Uncertainty interval shrinks as O(1/sqrt(n)) with sampled pages
    let se = 1.35 / (shape.pages_sampled.max(1) as f64).sqrt();
    let p_low = sigmoid(log_odds - 1.64 * se);
    let p_high = sigmoid(log_odds + 1.64 * se);

    let decision = if p_low >= 0.80 {
        "promote"
    } else if p_high <= 0.25 {
        "reject"
    } else {
        "uncertain"
    }
    .to_string();

    PosteriorEstimate {
        host: shape.host.clone(),
        pages_sampled: shape.pages_sampled,
        p_golden: (p_golden * 1000.0).round() / 1000.0,
        p_low: (p_low * 1000.0).round() / 1000.0,
        p_high: (p_high * 1000.0).round() / 1000.0,
        log_odds: (log_odds * 1000.0).round() / 1000.0,
        decision,
        contributions: c,
        shape: shape.clone(),
    }
}

fn feature_stats(vals: &[f64]) -> FeatureStats {
    if vals.is_empty() {
        return FeatureStats { mean: 0.0, std: 0.1 };
    }
    let med = median_of(vals);
    let devs: Vec<f64> = vals.iter().map(|&x| (x - med).abs()).collect();
    let mad = median_of(&devs) * 1.4826;
    let mean = vals.iter().sum::<f64>() / vals.len() as f64;
    let var = vals.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / vals.len() as f64;
    let std = mad.max(var.sqrt()).max(0.02);
    FeatureStats {
        mean: (med * 10000.0).round() / 10000.0,
        std: (std * 10000.0).round() / 10000.0,
    }
}

pub fn calibrate_from_cache(
    cache_path: &Path,
    reg: &Registry,
    out_path: &Path,
) -> Result<(GoldenBaseline, Vec<PosteriorEstimate>), String> {
    let conn = Connection::open(cache_path).map_err(|e| format!("open {}: {e}", cache_path.display()))?;
    let mut stmt = conn
        .prepare(
            "SELECT url, headers, body FROM http_cache
             WHERE status=200 AND url NOT LIKE '%.xml%' AND url NOT LIKE '%sitemap%'",
        )
        .map_err(|e| format!("query http_cache: {e}"))?;

    let golden_domains: HashSet<String> = reg
        .sites
        .iter()
        .map(|s| base_domain(&s.domain))
        .collect();

    let mut by_host: HashMap<String, Vec<PageShape>> = HashMap::new();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(|e| format!("iter http_cache: {e}"))?;

    for item in rows.flatten() {
        let (url, hdr_raw, body) = item;
        if body.len() < 1500 {
            continue;
        }
        let host = Url::parse(&url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default();
        if host.starts_with("api.") || !golden_domains.contains(&base_domain(&url)) {
            continue;
        }
        let hdrs: HashMap<String, String> = serde_json::from_str(&hdr_raw).unwrap_or_default();
        let html = decode_body(&body, &hdrs);
        if !html.trim_start().starts_with('<') {
            continue;
        }
        let shape = measure_page(&url, &html, Some(200));
        by_host.entry(host).or_default().push(shape);
    }

    let mut site_shapes = Vec::new();
    let mut total_pages = 0usize;
    for (host, pages) in &by_host {
        if pages.is_empty() {
            continue;
        }
        total_pages += pages.len();
        site_shapes.push(aggregate_site(host, pages));
    }
    if site_shapes.is_empty() {
        return Err("no cached golden HTML pages found to calibrate".into());
    }

    let col = |f: fn(&SiteShape) -> f64| -> Vec<f64> { site_shapes.iter().map(f).collect() };
    let baseline = GoldenBaseline {
        sites_calibrated: site_shapes.len(),
        pages_calibrated: total_pages,
        yield_ratio: feature_stats(&col(|s| s.median_yield)),
        block_entropy: feature_stats(&col(|s| s.median_block_entropy)),
        section_cv: feature_stats(&col(|s| s.median_section_cv)),
        burstiness: feature_stats(&col(|s| s.median_burstiness)),
        zlib_ratio: feature_stats(&col(|s| s.median_zlib_ratio)),
        mattr: feature_stats(&col(|s| s.median_mattr)),
        trigram_repeat: feature_stats(&col(|s| s.median_trigram_repeat)),
        specificity: feature_stats(&col(|s| s.median_specificity)),
        link_density: feature_stats(&col(|s| s.median_link_density)),
        affiliate_ratio: feature_stats(&col(|s| s.median_affiliate_ratio)),
        cross_page_dup: feature_stats(
            &site_shapes
                .iter()
                .filter(|s| s.pages_sampled >= 2)
                .map(|s| s.cross_page_dup)
                .collect::<Vec<_>>(),
        ),
        skeleton_cv: feature_stats(
            &site_shapes
                .iter()
                .filter(|s| s.pages_sampled >= 2)
                .map(|s| s.skeleton_cv)
                .collect::<Vec<_>>(),
        ),
    };

    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(&baseline).unwrap();
    std::fs::write(out_path, json).map_err(|e| format!("write {}: {e}", out_path.display()))?;

    let mut estimates: Vec<PosteriorEstimate> = site_shapes
        .iter()
        .map(|s| estimate_site(s, &baseline, -0.5))
        .collect();
    estimates.sort_by(|a, b| b.p_golden.total_cmp(&a.p_golden));
    Ok((baseline, estimates))
}

pub async fn measure_urls_sequential(
    host: &str,
    urls: &[String],
    fetcher: &Fetcher,
    baseline: &GoldenBaseline,
    prior_log_odds: f64,
) -> PosteriorEstimate {
    let mut pages = Vec::new();
    let mut last_est = None;
    for url in urls {
        let res = fetcher.get(url, GetOpts::default()).await;
        if res.robots_skipped {
            let p = PageShape {
                url: url.clone(),
                host: host.to_string(),
                verdict: "robots-denied".into(),
                clean_chars: 0,
                visible_chars: 0,
                yield_ratio: 0.0,
                block_entropy: 0.0,
                structured_ratio: 0.0,
                section_cv: 0.0,
                sentence_burstiness: 0.0,
                zlib_ratio: 1.0,
                mattr: 0.0,
                trigram_repeat: 0.0,
                specificity: 0.0,
                link_density: 0.0,
                affiliate_ratio: 0.0,
                heading_count: 0,
                list_count: 0,
                shingles: HashSet::new(),
            };
            pages.push(p);
        } else {
            pages.push(measure_page(url, &res.text, res.status));
        }
        let shape = aggregate_site(host, &pages);
        let est = estimate_site(&shape, baseline, prior_log_odds);
        let done = pages.len() >= 2 && est.decision != "uncertain";
        last_est = Some(est);
        if done {
            break;
        }
    }
    last_est.unwrap_or_else(|| {
        let shape = aggregate_site(host, &pages);
        estimate_site(&shape, baseline, prior_log_odds)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_golden_shape_from_programmatic_ai_slop() {
        let baseline = GoldenBaseline::default();

        // Real-style varied hardware review + technical spec table + varied sentence burstiness
        let golden_p1 = r#"<html><body><article><h1>LG C1 OLED 5000-Hour Lab Report</h1>
            <p>Burn-in happened slower than expected. After running CNN at 200 nits for 5,000 continuous hours across four calibration checkpoints, our colorimeter still measured delta-E under 1.8 on primary swatches, though subpixel wear became measurable on static red tickers.</p>
            <table><tr><th>Metric</th><th>0h</th><th>5000h</th></tr><tr><td>Peak nit</td><td>742</td><td>719</td></tr></table>
            <h2>Uniformity and Compensation Cycles</h2>
            <p>Short pixel refreshes run automatically every four hours of cumulative power-on time. When left unplugged overnight, compensation failed and banding persisted until two manual cycles completed cleanly.</p>
            <p>Compare <a href="https://www.rtings.com/tv/learn/oled-burn-in">our methodology</a> with earlier C9 data.</p>
            </article></body></html>"#;
        let golden_p2 = r#"<html><body><article><h1>Vaillant F28 Ignition Fault Guide</h1>
            <p>Code F28 means the boiler failed to light after three ignition attempts.</p>
            <h2>Gas Valve and Electrode Checks</h2>
            <ul><li>Verify meter supply pressure stays near 21 mbar during purge.</li><li>Inspect ionization lead resistance and condensate trap clearance before replacing the PCB.</li></ul>
            <p>Unlike intermittent F29 flame loss during operation, F28 locks out immediately at cold start when frozen condensate blocks the siphon.</p>
            </article></body></html>"#;

        let g_shape = aggregate_site(
            "golden.example",
            &[
                measure_page("https://golden.example/lg-c1", golden_p1, Some(200)),
                measure_page("https://golden.example/f28", golden_p2, Some(200)),
            ],
        );
        let g_est = estimate_site(&g_shape, &baseline, -0.5);

        // Programmatic AI slop: identical 2-sentence template + repeated boilerplate FAQ + affiliate links
        let mk_slop = |product: &str| {
            format!(
                r#"<html><body><article><h1>Best {product} Buying Guide 2026</h1>
                <p>In today's fast-paced world, choosing the right {product} can feel overwhelming. Fortunately, our team researched the top options to help you decide.</p>
                <h2>Why {product} Matters Today</h2>
                <p>Modern households rely on {product} every single day for convenience and peace of mind. Quality matters when selecting a dependable model for your family.</p>
                <h2>Top Picks You Can Buy</h2>
                <ul><li>Best Overall Pick — <a href="https://shop.other.com/go/1?tag=slop-20">Check Price</a></li><li>Best Budget Pick — <a href="https://amzn.to/abc?ref=slop">View Deal</a></li></ul>
                <h2>Frequently Asked Questions</h2>
                <p>How long does a {product} last? Most quality units last several years with proper care. Always compare features before making your final purchase decision.</p>
                </article></body></html>"#
            )
        };
        let s_shape = aggregate_site(
            "slop.example",
            &[
                measure_page("https://slop.example/best-toaster", &mk_slop("toaster"), Some(200)),
                measure_page("https://slop.example/best-blender", &mk_slop("blender"), Some(200)),
                measure_page("https://slop.example/best-kettle", &mk_slop("kettle"), Some(200)),
            ],
        );
        let s_est = estimate_site(&s_shape, &baseline, -0.5);

        assert!(
            g_est.p_golden > s_est.p_golden + 0.35,
            "expected golden ({:.3}) >> slop ({:.3})",
            g_est.p_golden,
            s_est.p_golden
        );
        assert!(g_est.p_golden > 0.70, "golden p={}", g_est.p_golden);
        assert!(s_est.p_golden < 0.35, "slop p={}", s_est.p_golden);
    }
}
