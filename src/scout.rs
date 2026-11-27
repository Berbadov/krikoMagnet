use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};
use scraper::{Html, Selector};
use serde::Serialize;
use serde_json::{Value, json};
use url::Url;

use crate::discover::{base_domain, parse_xml};
use crate::extract::classify_fetch;
use crate::http::{Fetcher, GetOpts, now_epoch};
use crate::measure::{
    GoldenBaseline, PosteriorEstimate, SiteShape, measure_urls_sequential_with_sample, sigmoid,
};
use crate::planner::{Registry, detect_language, load_registry};

/// Persistent Bayesian runtime trust & site calibration store (`data/sources.sqlite`).
pub struct SourceTrustStore {
    conn: Mutex<Connection>,
}

impl SourceTrustStore {
    pub fn default_path() -> PathBuf {
        PathBuf::from("data").join("sources.sqlite")
    }

    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
        let conn = Connection::open(path).map_err(|e| format!("open sources db: {e}"))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=15000;
             CREATE TABLE IF NOT EXISTS sources (
                 host TEXT PRIMARY KEY,
                 site_id TEXT,
                 alpha REAL NOT NULL DEFAULT 2.0,
                 beta REAL NOT NULL DEFAULT 2.0,
                 shape_log_odds REAL NOT NULL DEFAULT 0.0,
                 p_trust REAL NOT NULL DEFAULT 0.5,
                 decision TEXT NOT NULL DEFAULT 'uncertain',
                 pages_sampled INTEGER NOT NULL DEFAULT 0,
                 ok_fetches INTEGER NOT NULL DEFAULT 0,
                 blocked_fetches INTEGER NOT NULL DEFAULT 0,
                 reason TEXT,
                 updated REAL NOT NULL
             );",
        )
        .map_err(|e| format!("init sources schema: {e}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn record_estimate(&self, site_id: Option<&str>, est: &PosteriorEstimate) {
        let host = est.host.to_ascii_lowercase();
        let conn = self.conn.lock().unwrap();
        let prev: Option<(f64, f64, i64, i64)> = conn
            .query_row(
                "SELECT alpha, beta, ok_fetches, blocked_fetches FROM sources WHERE host=?1",
                params![host],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .ok()
            .flatten();
        let (alpha, beta, ok_n, blk_n) = prev.unwrap_or((2.0, 2.0, 0, 0));
        let access_adj =
            ((alpha / (alpha + beta)).ln() - (beta / (alpha + beta)).ln()).clamp(-3.0, 1.5);
        let p_trust = sigmoid(est.log_odds + access_adj * 0.6);
        let _ = conn.execute(
            "INSERT OR REPLACE INTO sources(host, site_id, alpha, beta, shape_log_odds, p_trust, decision, pages_sampled, ok_fetches, blocked_fetches, reason, updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11)",
            params![
                host,
                site_id,
                alpha,
                beta,
                est.log_odds,
                p_trust,
                est.decision,
                est.pages_sampled as i64,
                ok_n,
                blk_n,
                now_epoch()
            ],
        );
    }

    /// Conjugate Beta-Binomial update from a runtime fetch verdict ("success", "hard-block", "soft-block", "robots-denied").
    pub fn record_fetch_verdict(&self, host_or_url: &str, verdict: &str) {
        let host = base_domain(host_or_url);
        if host.is_empty() {
            return;
        }
        let is_ok = verdict == "success";
        let is_bad = matches!(verdict, "hard-block" | "soft-block" | "robots-denied");
        if !is_ok && !is_bad {
            return;
        }
        let conn = self.conn.lock().unwrap();
        let prev: Option<(Option<String>, f64, f64, f64, String, i64, i64, i64)> = conn
            .query_row(
                "SELECT site_id, alpha, beta, shape_log_odds, decision, pages_sampled, ok_fetches, blocked_fetches
                 FROM sources WHERE host=?1",
                params![host],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
            )
            .optional()
            .ok()
            .flatten();
        let (site_id, mut alpha, mut beta, shape_ll, prev_dec, sampled, mut ok_n, mut blk_n) =
            prev.unwrap_or((None, 2.0, 2.0, 0.0, "uncertain".into(), 0, 0, 0));
        if is_ok {
            alpha = (alpha + 1.0).min(80.0);
            ok_n += 1;
        } else {
            beta = (beta + 1.5).min(80.0);
            blk_n += 1;
        }
        let access_adj =
            ((alpha / (alpha + beta)).ln() - (beta / (alpha + beta)).ln()).clamp(-3.0, 1.5);
        let p_trust = sigmoid(shape_ll + access_adj * 0.6);
        let dec = if p_trust <= 0.22 && blk_n >= 3 {
            "reject".to_string()
        } else {
            prev_dec
        };
        let _ = conn.execute(
            "INSERT OR REPLACE INTO sources(host, site_id, alpha, beta, shape_log_odds, p_trust, decision, pages_sampled, ok_fetches, blocked_fetches, reason, updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL, ?11)",
            params![
                host,
                site_id,
                alpha,
                beta,
                shape_ll,
                p_trust,
                dec,
                sampled,
                ok_n,
                blk_n,
                now_epoch()
            ],
        );
    }

    pub fn trust_map(&self) -> HashMap<String, f64> {
        let mut out = HashMap::new();
        let conn = self.conn.lock().unwrap();
        if let Ok(mut stmt) = conn.prepare("SELECT host, p_trust FROM sources")
            && let Ok(rows) =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))
        {
            for (h, p) in rows.flatten() {
                let b = base_domain(&h);
                if !b.is_empty() {
                    out.insert(b, p);
                }
                out.insert(h.to_ascii_lowercase(), p);
            }
        }
        out
    }

    pub fn stats(&self) -> Value {
        let conn = self.conn.lock().unwrap();
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM sources", [], |r| r.get(0))
            .unwrap_or(0);
        let promoted: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sources WHERE decision='promote'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let rejected: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sources WHERE decision='reject'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        json!({
            "tracked": total,
            "promoted": promoted,
            "rejected": rejected,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ScoutReport {
    pub host: String,
    pub domain: String,
    pub site_id: String,
    pub language: String,
    pub discovered_method: String,
    pub sitemap_urls: Vec<String>,
    pub include_children: Vec<String>,
    pub candidates_found: usize,
    pub estimate: PosteriorEstimate,
    pub action: String,
    pub reason: String,
}

pub fn extract_sitemap_directives(robots_body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in robots_body.lines() {
        let trimmed = line.trim();
        if trimmed.len() > 8 && trimmed[..8].eq_ignore_ascii_case("sitemap:") {
            let url = trimmed[8..].trim();
            if url.starts_with("http://") || url.starts_with("https://") {
                if !out.iter().any(|x| x == url) {
                    out.push(url.to_string());
                }
            }
        }
    }
    out
}

fn score_child_sitemap(url: &str) -> i32 {
    let low = url.to_ascii_lowercase();
    let mut score = 0i32;
    for good in [
        "post", "article", "review", "guide", "wiki", "news", "blog", "content", "2026", "2025",
        "2024",
    ] {
        if low.contains(good) {
            score += 10;
        }
    }
    for bad in [
        "tag", "author", "category", "archive", "image", "video", "product", "shop", "store",
        "forum",
    ] {
        if low.contains(bad) {
            score -= 15;
        }
    }
    score
}

fn is_candidate_content_url(raw: &str, domain: &str) -> bool {
    let Ok(u) = Url::parse(raw) else {
        return false;
    };
    if !matches!(u.scheme(), "http" | "https") {
        return false;
    }
    if base_domain(raw) != domain {
        return false;
    }
    let path = u.path().to_ascii_lowercase();
    if path.is_empty() || path == "/" {
        return false;
    }
    for ext in [
        ".jpg", ".jpeg", ".png", ".gif", ".webp", ".svg", ".pdf", ".xml", ".css", ".js",
    ] {
        if path.ends_with(ext) {
            return false;
        }
    }
    for skip in [
        "/tag/",
        "/tags/",
        "/author/",
        "/category/",
        "/categories/",
        "/search",
        "/login",
        "/register",
        "/cart",
        "/checkout",
        "/account",
        "/page/",
        "/wp-content/",
    ] {
        if path.contains(skip) {
            return false;
        }
    }
    true
}

/// Picks `limit` evenly spaced URLs across candidates so site shape reflects real site-wide diversity.
pub fn select_diverse_sample(urls: &[String], limit: usize) -> Vec<String> {
    if urls.is_empty() || limit == 0 {
        return Vec::new();
    }
    if urls.len() <= limit {
        return urls.to_vec();
    }
    let mut picked = Vec::with_capacity(limit);
    let mut seen = HashSet::new();
    let step = urls.len() as f64 / limit as f64;
    for i in 0..limit {
        let idx = ((i as f64 + 0.5) * step).floor() as usize;
        let idx = idx.min(urls.len() - 1);
        if seen.insert(idx) {
            picked.push(urls[idx].clone());
        }
    }
    picked
}

fn suggest_site_id(domain: &str) -> String {
    let stem = domain.split('.').next().unwrap_or(domain);
    stem.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

fn infer_include_pattern(selected_children: &[String]) -> Vec<String> {
    for kw in ["post", "article", "review", "guide", "wiki", "content"] {
        if selected_children
            .iter()
            .all(|u| u.to_ascii_lowercase().contains(kw))
        {
            return vec![kw.to_string()];
        }
    }
    Vec::new()
}

fn rejected_report(host: &str, domain: String, reason: String) -> ScoutReport {
    ScoutReport {
        host: host.to_string(),
        domain,
        site_id: suggest_site_id(host),
        language: "en".into(),
        discovered_method: "none".into(),
        sitemap_urls: Vec::new(),
        include_children: Vec::new(),
        candidates_found: 0,
        estimate: PosteriorEstimate {
            host: host.to_string(),
            pages_sampled: 0,
            p_golden: 0.01,
            p_low: 0.0,
            p_high: 0.15,
            decision: "reject".into(),
            log_odds: -4.5,
            shape: SiteShape {
                host: host.to_string(),
                pages_sampled: 0,
                access_rate: 0.0,
                median_yield: 0.0,
                median_block_entropy: 0.0,
                median_structured_ratio: 0.0,
                median_section_cv: 0.0,
                median_burstiness: 0.0,
                median_zlib_ratio: 1.0,
                median_mattr: 0.0,
                median_trigram_repeat: 0.0,
                median_specificity: 0.0,
                median_link_density: 0.0,
                median_affiliate_ratio: 0.0,
                cross_page_dup: 0.0,
                skeleton_cv: 0.0,
            },
            contributions: HashMap::from([("access".into(), -4.0)]),
        },
        action: "reject".into(),
        reason,
    }
}

pub async fn scout_domain(
    target: &str,
    reg: &Registry,
    tranco: &HashMap<String, u32>,
    fetcher: &Fetcher,
    baseline: &GoldenBaseline,
    trust_store: Option<&SourceTrustStore>,
    sample_limit: usize,
    promote_path: Option<&Path>,
) -> ScoutReport {
    let raw_trimmed = target.trim();
    let seed_url = if raw_trimmed.starts_with("http://") || raw_trimmed.starts_with("https://") {
        raw_trimmed.to_string()
    } else {
        format!("https://{raw_trimmed}/")
    };
    let parsed_seed = Url::parse(&seed_url).ok();
    let initial_host = parsed_seed
        .as_ref()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_else(|| base_domain(raw_trimmed));
    let domain = base_domain(&initial_host);
    let mut origin = format!("https://{initial_host}");

    // 1. Fetch homepage politely (also resolves canonical host such as bare -> www. redirects)
    let home = fetcher.get(&format!("{origin}/"), GetOpts::default()).await;
    if home.robots_skipped {
        let rep = rejected_report(&initial_host, domain, "robots.txt disallows root".into());
        if let Some(ts) = trust_store {
            ts.record_estimate(None, &rep.estimate);
        }
        return maybe_persist_registry(rep, reg, promote_path);
    }
    let canonical_url_parsed = Url::parse(&home.final_url).ok();
    let host = canonical_url_parsed
        .as_ref()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or(initial_host);
    if !host.is_empty() {
        origin = format!("https://{host}");
    }

    if let Some(code) = home.status {
        if matches!(code, 401 | 403 | 429) {
            let rep = rejected_report(&host, domain, format!("HTTP {code} on homepage"));
            if let Some(ts) = trust_store {
                ts.record_estimate(None, &rep.estimate);
            }
            return maybe_persist_registry(rep, reg, promote_path);
        }
    } else {
        let err = home.error.clone().unwrap_or_else(|| "network error".into());
        let rep = rejected_report(&host, domain, format!("homepage fetch failed: {err}"));
        if let Some(ts) = trust_store {
            ts.record_estimate(None, &rep.estimate);
        }
        return maybe_persist_registry(rep, reg, promote_path);
    }

    if classify_fetch(home.status, &home.status_tag, home.text.len(), &home.text) == "soft-block"
        && home.text.to_ascii_lowercase().contains("just a moment")
    {
        let rep = rejected_report(&host, domain, "Cloudflare/JS challenge on homepage".into());
        if let Some(ts) = trust_store {
            ts.record_estimate(None, &rep.estimate);
        }
        return maybe_persist_registry(rep, reg, promote_path);
    }

    // 2. Discover public, robots-allowed discovery surfaces (sitemaps, RSS/Atom, WP REST)
    let pol = fetcher.robots_policy(&format!("{origin}/")).await;
    let mut sitemap_candidates = extract_sitemap_directives(&pol.body);
    for fallback_sm in ["/sitemap.xml", "/sitemap_index.xml"] {
        let u = format!("{origin}{fallback_sm}");
        if !sitemap_candidates.contains(&u) {
            sitemap_candidates.push(u);
        }
    }

    let mut urls: Vec<String> = Vec::new();
    let mut seen_urls: HashSet<String> = HashSet::new();
    let mut used_sitemaps: Vec<String> = Vec::new();
    let mut chosen_children: Vec<String> = Vec::new();
    let mut method_kind = "none".to_string();
    let mut sample_titles = String::new();

    for sm_url in &sitemap_candidates {
        if !pol.allowed(sm_url) {
            continue;
        }
        let res = fetcher
            .get(
                sm_url,
                GetOpts {
                    timeout_s: 20.0,
                    ..Default::default()
                },
            )
            .await;
        if !res.ok() || res.text.trim().is_empty() {
            continue;
        }
        let (kind, entries) = parse_xml(&res.text);
        if entries.is_empty() {
            continue;
        }
        if kind == "sitemapindex" {
            let mut kids: Vec<String> = entries
                .into_iter()
                .filter_map(|e| e.loc)
                .filter(|u| pol.allowed(u))
                .collect();
            kids.sort_by(|a, b| score_child_sitemap(b).cmp(&score_child_sitemap(a)));
            kids.truncate(3);
            if !kids.is_empty() {
                used_sitemaps.push(sm_url.clone());
                chosen_children = kids.clone();
                for child in kids {
                    let cres = fetcher
                        .get(
                            &child,
                            GetOpts {
                                timeout_s: 20.0,
                                ..Default::default()
                            },
                        )
                        .await;
                    if !cres.ok() {
                        continue;
                    }
                    let (_ck, centries) = parse_xml(&cres.text);
                    for ce in centries {
                        if let Some(loc) = ce.loc
                            && is_candidate_content_url(&loc, &domain)
                            && seen_urls.insert(loc.clone())
                        {
                            urls.push(loc);
                        }
                    }
                }
            }
        } else if kind == "urlset" {
            used_sitemaps.push(sm_url.clone());
            for e in entries {
                if let Some(loc) = e.loc
                    && is_candidate_content_url(&loc, &domain)
                    && seen_urls.insert(loc.clone())
                {
                    urls.push(loc);
                }
            }
        }
        if !urls.is_empty() {
            method_kind = "sitemap".into();
            break;
        }
    }

    // Fallback to RSS/Atom feeds linked in homepage HTML
    if urls.is_empty() && home.ok() {
        let doc = Html::parse_document(&home.text);
        let mut feed_urls = Vec::new();
        if let Ok(sel) = Selector::parse(
            r#"link[rel="alternate"][type*="rss"], link[rel="alternate"][type*="atom"], link[rel="alternate"][type*="xml"]"#,
        ) {
            for el in doc.select(&sel) {
                if let Some(href) = el.value().attr("href")
                    && let Ok(abs) = Url::parse(&home.final_url).and_then(|b| b.join(href))
                {
                    feed_urls.push(abs.to_string());
                }
            }
        }
        for feed in feed_urls.into_iter().take(2) {
            if !pol.allowed(&feed) {
                continue;
            }
            let fres = fetcher.get(&feed, GetOpts::default()).await;
            if !fres.ok() {
                continue;
            }
            let (_fk, fentries) = parse_xml(&fres.text);
            for fe in fentries {
                if let Some(link) = fe.link
                    && is_candidate_content_url(&link, &domain)
                    && seen_urls.insert(link.clone())
                {
                    if let Some(t) = fe.title {
                        sample_titles.push(' ');
                        sample_titles.push_str(&t);
                    }
                    urls.push(link);
                }
            }
            if !urls.is_empty() {
                used_sitemaps = vec![feed];
                method_kind = "rss".into();
                break;
            }
        }
    }

    // Documented WordPress REST API fallback
    if urls.is_empty() {
        let wp_url = format!("{origin}/wp-json/wp/v2/posts?per_page=12");
        if pol.allowed(&wp_url) {
            let wres = fetcher.get(&wp_url, GetOpts::default()).await;
            if wres.ok()
                && let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&wres.text)
            {
                for item in items {
                    if let Some(link) = item.get("link").and_then(|v| v.as_str())
                        && is_candidate_content_url(link, &domain)
                        && seen_urls.insert(link.to_string())
                    {
                        if let Some(t) = item.pointer("/title/rendered").and_then(|v| v.as_str()) {
                            sample_titles.push(' ');
                            sample_titles.push_str(t);
                        }
                        urls.push(link.to_string());
                    }
                }
                if !urls.is_empty() {
                    used_sitemaps = vec![format!(
                        "{origin}/wp-json/wp/v2/posts?search={{query}}&per_page=12"
                    )];
                    method_kind = "json_search".into();
                }
            }
        }
    }

    if urls.is_empty() {
        let rep = rejected_report(
            &host,
            domain,
            "no allowed sitemap, RSS feed, or documented WP API found".into(),
        );
        if let Some(ts) = trust_store {
            ts.record_estimate(None, &rep.estimate);
        }
        return maybe_persist_registry(rep, reg, promote_path);
    }

    let sample = select_diverse_sample(&urls, sample_limit.max(4));
    let prior_ll = tranco
        .get(&domain)
        .map(|&r| (0.3 - 0.18 * (r as f64).log10()).clamp(-1.0, 0.2))
        .unwrap_or(-0.7);
    let (est, sample_prose) =
        measure_urls_sequential_with_sample(&host, &sample, fetcher, baseline, prior_ll, 4).await;

    let lang = detect_language(&format!("{sample_titles} {sample_prose}")).to_string();
    let include_children = if chosen_children.is_empty() {
        Vec::new()
    } else {
        infer_include_pattern(&chosen_children)
    };
    let reason = if est.decision == "promote" {
        format!(
            "p_golden={:.3} (low={:.3} >= 0.80), burst={:.2}, dup={:.3}, aff={:.1}% across {} pages",
            est.p_golden,
            est.p_low,
            est.shape.median_burstiness,
            est.shape.cross_page_dup,
            est.shape.median_affiliate_ratio * 100.0,
            est.pages_sampled
        )
    } else if est.decision == "reject" {
        format!(
            "p_golden={:.3} (high={:.3} <= 0.25), access={:.0}%, burst={:.2}, dup={:.3}, aff={:.1}%",
            est.p_golden,
            est.p_high,
            est.shape.access_rate * 100.0,
            est.shape.median_burstiness,
            est.shape.cross_page_dup,
            est.shape.median_affiliate_ratio * 100.0
        )
    } else {
        format!(
            "uncertain credible interval [{:.2}, {:.2}] after {} pages",
            est.p_low, est.p_high, est.pages_sampled
        )
    };

    let rep = ScoutReport {
        host,
        domain,
        site_id: suggest_site_id(&base_domain(&est.host)),
        language: lang,
        discovered_method: method_kind,
        sitemap_urls: used_sitemaps,
        include_children,
        candidates_found: urls.len(),
        estimate: est.clone(),
        action: est.decision.clone(),
        reason,
    };
    if let Some(ts) = trust_store {
        let existing_id = reg
            .sites
            .iter()
            .find(|s| base_domain(&s.domain) == rep.domain)
            .map(|s| s.id.as_str());
        ts.record_estimate(existing_id.or(Some(rep.site_id.as_str())), &rep.estimate);
    }
    maybe_persist_registry(rep, reg, promote_path)
}

fn maybe_persist_registry(
    mut rep: ScoutReport,
    reg: &Registry,
    promote_path: Option<&Path>,
) -> ScoutReport {
    let Some(path) = promote_path else {
        return rep;
    };
    if reg
        .sites
        .iter()
        .any(|s| base_domain(&s.domain) == rep.domain)
    {
        rep.action = "already-registered".into();
        return rep;
    }
    if rep.estimate.decision == "reject" {
        if reg
            .skipped
            .iter()
            .any(|s| base_domain(&s.domain) == rep.domain)
        {
            rep.action = "already-skipped".into();
            return rep;
        }
        if append_skipped_yaml(path, &rep.domain, &rep.reason).is_ok() {
            rep.action = "skipped".into();
        }
        return rep;
    }
    if rep.estimate.decision != "promote" || rep.sitemap_urls.is_empty() {
        return rep;
    }
    let mut id = rep.site_id.clone();
    let mut n = 2usize;
    while reg.sites.iter().any(|s| s.id == id) {
        id = format!("{}{n}", rep.site_id);
        n += 1;
    }
    rep.site_id = id;
    if append_site_yaml(path, &rep).is_ok() {
        rep.action = "promoted".into();
    }
    rep
}

pub fn append_site_yaml(path: &Path, rep: &ScoutReport) -> Result<(), String> {
    let orig =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut entry = format!(
        "\n  - id: {}\n    name: {}\n    domain: {}\n    topics: [general]\n    language: {}\n    methods:\n      - type: {}\n",
        rep.site_id, rep.domain, rep.domain, rep.language, rep.discovered_method
    );
    if rep.discovered_method == "json_search" {
        entry.push_str(&format!(
            "        url: \"{}\"\n        title: title.rendered\n        snippet: excerpt.rendered\n        body_field: content.rendered\n        url_field: link\n        date: date\n        max_results: 20\n",
            rep.sitemap_urls[0]
        ));
    } else {
        entry.push_str("        urls:\n");
        for u in &rep.sitemap_urls {
            entry.push_str(&format!("          - {u}\n"));
        }
        if !rep.include_children.is_empty() {
            let quoted: Vec<String> = rep
                .include_children
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect();
            entry.push_str(&format!(
                "        include_children: [{}]\n",
                quoted.join(", ")
            ));
            entry.push_str("        max_sitemaps: 3\n");
        }
        entry.push_str("        max_urls: 15000\n        max_results: 30\n");
    }
    let safe_reason = rep.reason.replace('"', "'");
    entry.push_str(&format!(
        "    notes: \"Promoted automatically by magnet scout ({safe_reason}).\"\n"
    ));

    let updated = if let Some(pos) = orig.find("\nopen_web:") {
        let (head, tail) = orig.split_at(pos);
        format!("{head}{entry}{tail}")
    } else if let Some(pos) = orig.find("\nskipped:") {
        let (head, tail) = orig.split_at(pos);
        format!("{head}{entry}{tail}")
    } else {
        format!("{orig}{entry}")
    };

    std::fs::write(path, &updated).map_err(|e| format!("write {}: {e}", path.display()))?;
    if let Err(err) = load_registry(path) {
        let _ = std::fs::write(path, &orig);
        return Err(format!("rolled back invalid YAML edit: {err}"));
    }
    Ok(())
}

pub fn append_skipped_yaml(path: &Path, domain: &str, reason: &str) -> Result<(), String> {
    let orig =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let safe_reason = reason.replace('"', "'");
    let block = format!("\n  - domain: {domain}\n    reason: \"{safe_reason}\"");
    let updated = if orig.contains("\nskipped:") {
        format!("{orig}{block}\n")
    } else {
        format!("{orig}\nskipped:{block}\n")
    };
    std::fs::write(path, &updated).map_err(|e| format!("write {}: {e}", path.display()))?;
    if let Err(err) = load_registry(path) {
        let _ = std::fs::write(path, &orig);
        return Err(format!("rolled back skipped edit: {err}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_store_updates_from_estimate_and_runtime_blocks() {
        let dir = std::env::temp_dir().join(format!("magnet-trust-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = SourceTrustStore::open(&dir.join("sources.sqlite")).unwrap();

        let est = PosteriorEstimate {
            host: "www.example.org".into(),
            pages_sampled: 5,
            p_golden: 0.92,
            p_low: 0.82,
            p_high: 0.97,
            decision: "promote".into(),
            log_odds: 2.44,
            shape: SiteShape {
                host: "www.example.org".into(),
                pages_sampled: 5,
                access_rate: 1.0,
                median_yield: 0.7,
                median_block_entropy: 0.5,
                median_structured_ratio: 0.1,
                median_section_cv: 0.5,
                median_burstiness: 0.6,
                median_zlib_ratio: 0.55,
                median_mattr: 0.85,
                median_trigram_repeat: 0.02,
                median_specificity: 0.05,
                median_link_density: 0.1,
                median_affiliate_ratio: 0.0,
                cross_page_dup: 0.01,
                skeleton_cv: 0.4,
            },
            contributions: HashMap::new(),
        };
        store.record_estimate(Some("example"), &est);
        let p0 = store.trust_map()["example.org"];
        assert!(p0 > 0.85, "expected high trust, got {p0}");

        for _ in 0..8 {
            store.record_fetch_verdict("https://www.example.org/foo", "hard-block");
        }
        let p1 = store.trust_map()["example.org"];
        assert!(p1 < p0 - 0.10, "expected runtime block decay: {p0} -> {p1}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_directives_sample_spread_and_registry_promote() {
        let robots = "User-agent: *\nDisallow: /search\nSitemap: https://a.example/sitemap.xml\n";
        assert_eq!(
            extract_sitemap_directives(robots),
            vec!["https://a.example/sitemap.xml"]
        );

        let urls: Vec<String> = (0..100)
            .map(|i| format!("https://a.example/p/{i}"))
            .collect();
        let samp = select_diverse_sample(&urls, 5);
        assert_eq!(samp.len(), 5);
        assert!(samp[0] != samp[4] && samp[4].contains("/p/9"));

        let dir = std::env::temp_dir().join(format!("magnet-promote-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let reg_copy = dir.join("registry.yaml");
        std::fs::copy("registry.yaml", &reg_copy).unwrap();
        let reg = load_registry(&reg_copy).unwrap();
        let mut rep = rejected_report("golden.example", "golden.example".into(), "good".into());
        rep.discovered_method = "sitemap".into();
        rep.sitemap_urls = vec!["https://golden.example/sitemap.xml".into()];
        rep.estimate.decision = "promote".into();
        rep.estimate.p_low = 0.88;
        let out = maybe_persist_registry(rep, &reg, Some(&reg_copy));
        assert_eq!(out.action, "promoted");
        let reloaded = load_registry(&reg_copy).unwrap();
        assert!(reloaded.sites.iter().any(|s| s.domain == "golden.example"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
