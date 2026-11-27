use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};

use chrono::{Duration, NaiveDate, Utc};
use regex::Regex;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::Deserialize;
use serde_json::{Value, json};
use url::{Host, Url};

use crate::discover::base_domain;
use crate::planner::{Registry, Site};
use crate::text::fold;

const PERSONAL_SEGMENTS: &[&str] = &[
    "account",
    "accounts",
    "profile",
    "user",
    "users",
    "member",
    "login",
    "signin",
    "signup",
    "cart",
    "basket",
    "checkout",
    "order",
    "orders",
    "siparis",
    "sepet",
    "hesabim",
    "hesap",
    "uye",
    "uyelik",
    "odeme",
    "payment",
    "invoice",
    "fatura",
    "track",
    "tracking",
    "unsubscribe",
    "reset",
    "token",
    "session",
    "me",
];
const PRIVATE_TLDS: &[&str] = &[
    "local",
    "internal",
    "localhost",
    "localdomain",
    "lan",
    "home",
    "corp",
    "intranet",
    "arpa",
    "test",
    "invalid",
    "onion",
    "example",
];
const MAX_URL_CHARS: usize = 300;
const MAX_KEY_TOKENS: usize = 8;
const MAX_QUERY_TOKENS: usize = 32;
const MAX_COUNT: u32 = 1000;
const MAX_ERRORS: usize = 50;
const MAX_INJECTED: usize = 5;
const PRIOR: f64 = 2.0;
pub const MAX_HINT_DOMAINS: usize = 20;

static EMAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[a-z0-9._%+-]+@[a-z0-9-]+(?:\.[a-z0-9-]+)+").expect("valid email regex")
});
static PHONE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\+\s*\d(?:[\s().-]*\d){7,}").expect("valid phone regex"));
static IBAN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[a-z]{2}\d{2}(?:\s?[a-z0-9]){11,30}").expect("valid iban regex")
});

pub fn today() -> NaiveDate {
    Utc::now().date_naive()
}

fn day_str(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn key_text(folded: &str) -> String {
    folded
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || " .+-/_".contains(c) {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn product_key(raw: &str) -> Option<String> {
    let folded = fold(raw);
    let iban = IBAN_RE
        .find_iter(&folded)
        .any(|m| m.as_str().bytes().filter(|b| b.is_ascii_digit()).count() >= 10);
    if folded.contains("http") || EMAIL_RE.is_match(&folded) || PHONE_RE.is_match(&folded) || iban {
        return None;
    }
    let key = key_text(&folded);
    let len = key.chars().count();
    ((3..=120).contains(&len) && key.split(' ').count() <= MAX_KEY_TOKENS).then_some(key)
}

fn query_keys(query: &str) -> Vec<String> {
    let norm = key_text(&fold(query));
    let toks: Vec<&str> = norm
        .split(' ')
        .filter(|t| !t.is_empty())
        .take(MAX_QUERY_TOKENS)
        .collect();
    let mut out: Vec<String> = Vec::new();
    for n in 1..=MAX_KEY_TOKENS.min(toks.len()) {
        for w in toks.windows(n) {
            let k = w.join(" ");
            if (3..=120).contains(&k.chars().count()) && !out.contains(&k) {
                out.push(k);
            }
        }
    }
    out
}

fn public_domain(raw: &str) -> Option<String> {
    let d = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    let d = d.strip_prefix("www.").unwrap_or(&d);
    let labels: Vec<&str> = d.split('.').collect();
    let tld = *labels.last()?;
    let ok = labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        && tld.len() >= 2
        && (tld.bytes().all(|b| b.is_ascii_alphabetic()) || tld.starts_with("xn--"))
        && !PRIVATE_TLDS.contains(&tld);
    ok.then(|| d.to_string())
}

fn mixes_letters_digits(s: &str) -> bool {
    s.bytes().any(|b| b.is_ascii_alphabetic()) && s.bytes().any(|b| b.is_ascii_digit())
}

fn is_token(part: &str) -> bool {
    let hex: String = part.chars().filter(|c| *c != '-').collect();
    if hex.len() >= 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return true;
    }
    part.len() >= 32
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && part
            .split(['-', '_'])
            .any(|w| w.len() >= 16 && mixes_letters_digits(w))
}

fn personal_segment(seg: &str) -> bool {
    let s = seg.to_ascii_lowercase();
    let stem = s.split('.').next().unwrap_or("");
    s.contains('@')
        || s.contains("%40")
        || s.starts_with("my-")
        || PERSONAL_SEGMENTS.contains(&stem)
        || s.split('.').any(is_token)
}

pub fn sanitize_url(raw: &str) -> Option<String> {
    let u = Url::parse(raw.trim()).ok()?;
    if !matches!(u.scheme(), "http" | "https")
        || !u.username().is_empty()
        || u.password().is_some()
        || u.port().is_some()
    {
        return None;
    }
    let host = match u.host()? {
        Host::Domain(d) => public_domain(d)?,
        _ => return None,
    };
    let path = u.path();
    if path.split('/').any(personal_segment) {
        return None;
    }
    let out = format!("{}://{host}{path}", u.scheme());
    (out.chars().count() <= MAX_URL_CHARS).then_some(out)
}

pub fn site_for_url<'a>(reg: &'a Registry, url: &str) -> Option<&'a Site> {
    let host = Url::parse(url).ok()?.host_str()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    reg.sites.iter().find(|s| {
        let d = s.domain.to_ascii_lowercase();
        let d = d.strip_prefix("www.").unwrap_or(&d);
        host == d || host.ends_with(&format!(".{d}"))
    })
}

fn hint_domain(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.contains(['/', '@', ':', '?', '#', ' ']) {
        return None;
    }
    public_domain(raw).map(|d| base_domain(&d))
}

#[derive(Default)]
pub struct Hints {
    prefer: HashSet<String>,
    avoid: HashSet<String>,
    lang: Option<String>,
}

impl Hints {
    pub fn new(prefer: &[String], avoid: &[String], lang: Option<&str>) -> Result<Self, String> {
        if prefer.len() > MAX_HINT_DOMAINS || avoid.len() > MAX_HINT_DOMAINS {
            return Err(format!(
                "prefer_domains and avoid_domains take at most {MAX_HINT_DOMAINS} domains each"
            ));
        }
        let lang = match lang.map(|l| l.trim().to_ascii_lowercase()) {
            None => None,
            Some(l) if l.len() == 2 && l.bytes().all(|b| b.is_ascii_lowercase()) => Some(l),
            Some(_) => return Err("lang must be a two-letter language code".into()),
        };
        Ok(Self {
            prefer: prefer.iter().filter_map(|d| hint_domain(d)).collect(),
            avoid: avoid.iter().filter_map(|d| hint_domain(d)).collect(),
            lang,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.prefer.is_empty() && self.avoid.is_empty() && self.lang.is_none()
    }

    pub fn factor(&self, domain: &str, lang: Option<&str>) -> f64 {
        let mut f = 1.0f64;
        if self.prefer.contains(domain) {
            f *= 1.2;
        }
        if self.avoid.contains(domain) {
            f *= 0.7;
        }
        if self.lang.is_some() && self.lang.as_deref() == lang {
            f *= 1.1;
        }
        f.clamp(0.7, 1.3)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Counts {
    pub useful: f64,
    pub unhelpful: f64,
    pub served: f64,
}

impl Counts {
    fn add(&mut self, o: Counts) {
        self.useful += o.useful;
        self.unhelpful += o.unhelpful;
        self.served += o.served;
    }

    pub fn usefulness(&self) -> f64 {
        (self.useful + PRIOR) / (self.useful + self.unhelpful + 2.0 * PRIOR)
    }

    fn multiplier(&self) -> f64 {
        let p = self.usefulness();
        if p >= 0.5 {
            1.0 + (p - 0.5) * 0.5
        } else {
            1.0 - (0.5 - p) * 0.4
        }
    }
}

pub struct LearnedUrl {
    pub url: String,
    pub product: String,
    pub counts: Counts,
}

#[derive(Default)]
pub struct Boosts {
    domains: HashMap<String, Counts>,
    urls: HashMap<String, Counts>,
}

impl Boosts {
    pub fn factor(&self, url: &str) -> f64 {
        let d = self
            .domains
            .get(&base_domain(url))
            .map_or(1.0, Counts::multiplier);
        let u = sanitize_url(url)
            .and_then(|k| self.urls.get(&k))
            .map_or(1.0, Counts::multiplier);
        (d * u).clamp(0.75, 1.4)
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Problems,
    Recall,
    Review,
    Manual,
    Repair,
    Price,
    Compare,
    Other,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Problems => "problems",
            Kind::Recall => "recall",
            Kind::Review => "review",
            Kind::Manual => "manual",
            Kind::Repair => "repair",
            Kind::Price => "price",
            Kind::Compare => "compare",
            Kind::Other => "other",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Signal {
    Useful,
    Unhelpful,
    Served,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Outcome {
    Hit,
    Miss,
    Unknown,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    product: String,
    kind: Kind,
    lang: String,
    country: String,
    url: String,
    signal: Signal,
    count: u32,
    day: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TopUrl {
    url: String,
    #[serde(default, rename = "domain")]
    _domain: Option<String>,
    #[serde(default, rename = "title")]
    _title: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CorpusRow {
    product: String,
    kind: Kind,
    language: String,
    country: String,
    #[serde(rename = "outcome")]
    _outcome: Outcome,
    count: u32,
    top_urls: Vec<TopUrl>,
    last_seen: String,
}

struct Obs {
    product: String,
    url: String,
    domain: String,
    kind: Kind,
    lang: String,
    country: String,
    signal: Signal,
    weight: f64,
    day: NaiveDate,
}

#[derive(Clone, Copy)]
pub struct LearnConfig {
    pub min_count: f64,
    pub retention_days: i64,
    pub half_life_days: f64,
    pub ranking: bool,
    pub scout_min_useful: f64,
}

impl Default for LearnConfig {
    fn default() -> Self {
        Self {
            min_count: 3.0,
            retention_days: 180,
            half_life_days: 90.0,
            ranking: true,
            scout_min_useful: 10.0,
        }
    }
}

impl LearnConfig {
    pub fn from_env() -> Self {
        let d = Self::default();
        let ranking = std::env::var("MAGNET_LEARN_RANKING")
            .map(|v| {
                !matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "0" | "off" | "false" | "no"
                )
            })
            .unwrap_or(true);
        Self {
            min_count: crate::env_num("MAGNET_LEARN_MIN_COUNT", d.min_count).max(1.0),
            retention_days: crate::env_num("MAGNET_LEARN_RETENTION_DAYS", d.retention_days).max(1),
            half_life_days: crate::env_num("MAGNET_LEARN_HALF_LIFE_DAYS", d.half_life_days)
                .max(1.0),
            ranking,
            scout_min_useful: crate::env_num("MAGNET_LEARN_SCOUT_MIN_USEFUL", d.scout_min_useful)
                .max(d.min_count),
        }
    }
}

fn lang_code(raw: &str) -> Result<String, &'static str> {
    let l = raw.trim().to_ascii_lowercase();
    if l.len() == 2 && l.bytes().all(|b| b.is_ascii_lowercase()) {
        Ok(l)
    } else {
        Err("lang must be a two-letter code")
    }
}

fn country_code(raw: &str) -> Result<String, &'static str> {
    let c = raw.trim().to_ascii_uppercase();
    if c.is_empty() || (c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase())) {
        Ok(c)
    } else {
        Err("country must be empty or a two-letter code")
    }
}

const VISIBLE: &str = "(useful + unhelpful + served) >= ?1";

pub struct LearnStore {
    conn: Mutex<Connection>,
    pub cfg: LearnConfig,
    pub path: PathBuf,
}

impl LearnStore {
    pub fn default_path() -> PathBuf {
        PathBuf::from("data").join("learn.sqlite")
    }

    pub fn open(path: &Path, cfg: LearnConfig) -> Result<Self, String> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
        let conn = Connection::open(path).map_err(|e| format!("open learn db: {e}"))?;
        conn.execute_batch(
            "PRAGMA auto_vacuum=INCREMENTAL;
             PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=30000;
             CREATE TABLE IF NOT EXISTS learned_urls(
                 product_key TEXT NOT NULL, url TEXT NOT NULL, domain TEXT NOT NULL,
                 kind TEXT NOT NULL, lang TEXT NOT NULL, country TEXT NOT NULL,
                 useful REAL NOT NULL DEFAULT 0, unhelpful REAL NOT NULL DEFAULT 0,
                 served REAL NOT NULL DEFAULT 0, first_day TEXT NOT NULL, last_day TEXT NOT NULL,
                 UNIQUE(product_key, url, kind, lang, country));
             CREATE INDEX IF NOT EXISTS learned_urls_url ON learned_urls(url);
             CREATE TABLE IF NOT EXISTS domain_stats(
                 domain TEXT PRIMARY KEY, useful REAL NOT NULL DEFAULT 0,
                 unhelpful REAL NOT NULL DEFAULT 0, served REAL NOT NULL DEFAULT 0,
                 last_day TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS learn_meta(name TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .map_err(|e| format!("init learn db: {e}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
            cfg,
            path: path.to_path_buf(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn parse_day(&self, raw: &str, today: NaiveDate) -> Result<NaiveDate, &'static str> {
        let d = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
            .ok()
            .filter(|_| raw.len() == 10)
            .ok_or("day must be YYYY-MM-DD")?;
        if d > today + Duration::days(1) {
            return Err("day is in the future");
        }
        if (today - d).num_days() > self.cfg.retention_days {
            return Err("day is older than the retention window");
        }
        Ok(d)
    }

    fn weight(&self, count: u32, day: NaiveDate, today: NaiveDate) -> Result<f64, &'static str> {
        if !(1..=MAX_COUNT).contains(&count) {
            return Err("count must be 1..=1000");
        }
        let age = (today - day).num_days().max(0) as f64;
        Ok(count as f64 * 0.5f64.powf(age / self.cfg.half_life_days))
    }

    fn observations(&self, v: Value, today: NaiveDate) -> Result<Vec<Obs>, &'static str> {
        const SHAPE: &str = "unknown field, missing field or wrong type";
        if v.get("topUrls").is_some() {
            let row: CorpusRow = serde_json::from_value(v).map_err(|_| SHAPE)?;
            let product = product_key(&row.product).ok_or("product rejected")?;
            let lang = lang_code(&row.language)?;
            let country = country_code(&row.country)?;
            let day = self.parse_day(row.last_seen.get(..10).unwrap_or(""), today)?;
            let weight = self.weight(row.count, day, today)?;
            let mut seen = HashSet::new();
            let out: Vec<Obs> = row
                .top_urls
                .iter()
                .filter_map(|t| sanitize_url(&t.url))
                .filter(|u| seen.insert(u.clone()))
                .map(|url| Obs {
                    product: product.clone(),
                    domain: base_domain(&url),
                    url,
                    kind: row.kind,
                    lang: lang.clone(),
                    country: country.clone(),
                    signal: Signal::Served,
                    weight,
                    day,
                })
                .collect();
            if out.is_empty() {
                return Err("no acceptable url");
            }
            return Ok(out);
        }
        let it: Item = serde_json::from_value(v).map_err(|_| SHAPE)?;
        let product = product_key(&it.product).ok_or("product rejected")?;
        let url = sanitize_url(&it.url).ok_or("url rejected")?;
        let day = self.parse_day(&it.day, today)?;
        Ok(vec![Obs {
            product,
            domain: base_domain(&url),
            url,
            kind: it.kind,
            lang: lang_code(&it.lang)?,
            country: country_code(&it.country)?,
            signal: it.signal,
            weight: self.weight(it.count, day, today)?,
            day,
        }])
    }

    pub fn ingest(&self, body: &str, today: NaiveDate) -> Result<Value, String> {
        let items: Vec<(usize, Option<Value>)> = if body.trim_start().starts_with('[') {
            match serde_json::from_str::<Vec<Value>>(body) {
                Ok(list) => list
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| (i + 1, Some(v)))
                    .collect(),
                Err(_) => vec![(1, None)],
            }
        } else {
            body.lines()
                .enumerate()
                .filter(|(_, l)| !l.trim().is_empty())
                .map(|(i, l)| (i + 1, serde_json::from_str(l).ok()))
                .collect()
        };
        let mut conn = self.lock();
        decay(&conn, today, self.cfg.half_life_days).map_err(|e| format!("decay: {e}"))?;
        let tx = conn.transaction().map_err(|e| format!("tx: {e}"))?;
        let (mut accepted, mut rejected, mut errors) = (0usize, 0usize, Vec::new());
        for (line, item) in items {
            let obs = item
                .ok_or("invalid json")
                .and_then(|v| self.observations(v, today));
            match obs {
                Ok(list) => {
                    for o in &list {
                        upsert(&tx, o).map_err(|e| format!("write: {e}"))?;
                    }
                    accepted += 1;
                }
                Err(reason) => {
                    rejected += 1;
                    if errors.len() < MAX_ERRORS {
                        errors.push(json!({ "line": line, "reason": reason }));
                    }
                }
            }
        }
        tx.commit().map_err(|e| format!("commit: {e}"))?;
        Ok(json!({ "accepted": accepted, "rejected": rejected, "errors": errors }))
    }

    pub fn prune(&self, today: NaiveDate) -> Value {
        let cutoff = day_str(today - Duration::days(self.cfg.retention_days));
        let conn = self.lock();
        let urls = conn
            .execute(
                "DELETE FROM learned_urls WHERE last_day < ?1",
                params![cutoff],
            )
            .unwrap_or(0);
        let domains = conn
            .execute(
                "DELETE FROM domain_stats WHERE last_day < ?1",
                params![cutoff],
            )
            .unwrap_or(0);
        let days = decay(&conn, today, self.cfg.half_life_days).unwrap_or(0);
        let _ = conn.execute_batch("PRAGMA incremental_vacuum; PRAGMA wal_checkpoint(TRUNCATE);");
        json!({ "urls_expired": urls, "domains_expired": domains, "decay_days": days })
    }

    pub fn record_served(&self, domains: &HashSet<String>, today: NaiveDate) {
        let day = day_str(today);
        let conn = self.lock();
        for d in domains.iter().filter_map(|d| public_domain(d)) {
            let _ = conn.execute(
                "INSERT INTO domain_stats(domain, served, last_day) VALUES (?1, 1, ?2)
                 ON CONFLICT(domain) DO UPDATE SET served = served + 1,
                     last_day = MAX(last_day, excluded.last_day)",
                params![base_domain(&d), day],
            );
        }
    }

    fn domain_counts(&self) -> HashMap<String, Counts> {
        let conn = self.lock();
        let mut out = HashMap::new();
        let sql =
            format!("SELECT domain, useful, unhelpful, served FROM domain_stats WHERE {VISIBLE}");
        if let Ok(mut stmt) = conn.prepare(&sql)
            && let Ok(rows) = stmt.query_map(params![self.cfg.min_count], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    Counts {
                        useful: r.get(1)?,
                        unhelpful: r.get(2)?,
                        served: r.get(3)?,
                    },
                ))
            })
        {
            out.extend(rows.flatten());
        }
        out
    }

    pub fn for_query(&self, query: &str) -> Vec<LearnedUrl> {
        let keys = query_keys(query);
        if keys.is_empty() {
            return Vec::new();
        }
        let marks = vec!["?"; keys.len()].join(",");
        let sql = format!(
            "SELECT product_key, url, SUM(useful), SUM(unhelpful), SUM(served) FROM learned_urls
             WHERE {VISIBLE} AND product_key IN ({marks}) GROUP BY product_key, url"
        );
        let mut args: Vec<SqlValue> = vec![SqlValue::Real(self.cfg.min_count)];
        args.extend(keys.into_iter().map(SqlValue::Text));
        let rows: Vec<(String, String, Counts)> = {
            let conn = self.lock();
            let Ok(mut stmt) = conn.prepare(&sql) else {
                return Vec::new();
            };
            stmt.query_map(params_from_iter(args), |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    Counts {
                        useful: r.get(2)?,
                        unhelpful: r.get(3)?,
                        served: r.get(4)?,
                    },
                ))
            })
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
        };
        let longest = rows
            .iter()
            .map(|(p, _, _)| p.split(' ').count())
            .max()
            .unwrap_or(0);
        let mut by_url: HashMap<String, LearnedUrl> = HashMap::new();
        for (product, url, counts) in rows {
            if product.split(' ').count() < longest {
                continue;
            }
            by_url
                .entry(url.clone())
                .or_insert_with(|| LearnedUrl {
                    url,
                    product,
                    counts: Counts::default(),
                })
                .counts
                .add(counts);
        }
        let mut out: Vec<LearnedUrl> = by_url.into_values().collect();
        out.sort_by(|a, b| {
            b.counts
                .usefulness()
                .total_cmp(&a.counts.usefulness())
                .then(b.counts.served.total_cmp(&a.counts.served))
                .then(a.url.cmp(&b.url))
        });
        out
    }

    pub fn boosts(&self, query: &str) -> (Boosts, Vec<LearnedUrl>) {
        if !self.cfg.ranking {
            return (Boosts::default(), Vec::new());
        }
        let learned = self.for_query(query);
        let boosts = Boosts {
            domains: self.domain_counts(),
            urls: learned.iter().map(|l| (l.url.clone(), l.counts)).collect(),
        };
        let inject = learned
            .into_iter()
            .filter(|l| l.counts.unhelpful <= l.counts.useful)
            .take(MAX_INJECTED)
            .collect();
        (boosts, inject)
    }

    pub fn stats(&self) -> Value {
        let k = self.cfg.min_count;
        let conn = self.lock();
        let count = |sql: &str| -> i64 {
            conn.query_row(&format!("{sql} WHERE {VISIBLE}"), params![k], |r| r.get(0))
                .unwrap_or(0)
        };
        let urls = count("SELECT COUNT(DISTINCT url) FROM learned_urls");
        let products = count("SELECT COUNT(DISTINCT product_key) FROM learned_urls");
        let domains = count("SELECT COUNT(*) FROM domain_stats");
        drop(conn);
        let mut top: Vec<(String, Counts)> = self.domain_counts().into_iter().collect();
        top.sort_by(|a, b| {
            b.1.usefulness()
                .total_cmp(&a.1.usefulness())
                .then(a.0.cmp(&b.0))
        });
        let top_domains: Vec<Value> = top
            .into_iter()
            .take(10)
            .map(|(d, c)| {
                json!({
                    "domain": d,
                    "useful": c.useful.round(),
                    "unhelpful": c.unhelpful.round(),
                    "served": c.served.round(),
                    "usefulness": (c.usefulness() * 1000.0).round() / 1000.0,
                })
            })
            .collect();
        json!({
            "min_count": k,
            "urls": urls,
            "products": products,
            "domains": domains,
            "top_domains": top_domains,
        })
    }

    pub fn export(&self) -> Vec<String> {
        let conn = self.lock();
        let sql = format!(
            "SELECT product_key, kind, lang, country, url, useful, unhelpful, served, last_day
             FROM learned_urls WHERE {VISIBLE} ORDER BY product_key, url, kind, lang, country"
        );
        let Ok(mut stmt) = conn.prepare(&sql) else {
            return Vec::new();
        };
        type Row = (
            String,
            String,
            String,
            String,
            String,
            f64,
            f64,
            f64,
            String,
        );
        let rows: Vec<Row> = stmt
            .query_map(params![self.cfg.min_count], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            })
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default();
        let mut out = Vec::new();
        for (product, kind, lang, country, url, useful, unhelpful, served, day) in rows {
            for (signal, n) in [
                ("useful", useful),
                ("unhelpful", unhelpful),
                ("served", served),
            ] {
                let count = n.round().min(MAX_COUNT as f64);
                if count >= 1.0 {
                    out.push(
                        json!({
                            "product": product, "kind": kind, "lang": lang, "country": country,
                            "url": url, "signal": signal, "count": count as u32, "day": day,
                        })
                        .to_string(),
                    );
                }
            }
        }
        out
    }
}

fn upsert(conn: &Connection, o: &Obs) -> rusqlite::Result<()> {
    let (u, h, s) = match o.signal {
        Signal::Useful => (o.weight, 0.0, 0.0),
        Signal::Unhelpful => (0.0, o.weight, 0.0),
        Signal::Served => (0.0, 0.0, o.weight),
    };
    let day = day_str(o.day);
    conn.execute(
        "INSERT INTO learned_urls(product_key, url, domain, kind, lang, country,
             useful, unhelpful, served, first_day, last_day)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
         ON CONFLICT(product_key, url, kind, lang, country) DO UPDATE SET
             useful = useful + excluded.useful, unhelpful = unhelpful + excluded.unhelpful,
             served = served + excluded.served, first_day = MIN(first_day, excluded.first_day),
             last_day = MAX(last_day, excluded.last_day)",
        params![
            o.product,
            o.url,
            o.domain,
            o.kind.as_str(),
            o.lang,
            o.country,
            u,
            h,
            s,
            day
        ],
    )?;
    conn.execute(
        "INSERT INTO domain_stats(domain, useful, unhelpful, served, last_day)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(domain) DO UPDATE SET
             useful = useful + excluded.useful, unhelpful = unhelpful + excluded.unhelpful,
             served = served + excluded.served, last_day = MAX(last_day, excluded.last_day)",
        params![o.domain, u, h, s, day],
    )?;
    Ok(())
}

fn decay(conn: &Connection, today: NaiveDate, half_life: f64) -> rusqlite::Result<i64> {
    let last: Option<String> = conn
        .query_row(
            "SELECT value FROM learn_meta WHERE name = 'decayed_day'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let prev = last.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok());
    let days = prev.map_or(0, |d| (today - d).num_days());
    if days > 0 {
        let f = 0.5f64.powf(days as f64 / half_life);
        for table in ["learned_urls", "domain_stats"] {
            conn.execute(
                &format!(
                    "UPDATE {table} SET useful = useful * ?1, unhelpful = unhelpful * ?1, served = served * ?1"
                ),
                params![f],
            )?;
        }
    }
    if prev.is_none() || days > 0 {
        conn.execute(
            "INSERT OR REPLACE INTO learn_meta(name, value) VALUES ('decayed_day', ?1)",
            params![day_str(today)],
        )?;
    }
    Ok(days.max(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (LearnStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("magnet-learn-{}", rand::random::<u64>()));
        let s = LearnStore::open(&dir.join("learn.sqlite"), LearnConfig::default()).unwrap();
        (s, dir)
    }

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn item(product: &str, url: &str, signal: &str, count: u32, d: &str) -> String {
        json!({
            "product": product, "kind": "review", "lang": "en", "country": "TR",
            "url": url, "signal": signal, "count": count, "day": d,
        })
        .to_string()
    }

    #[test]
    fn sanitizer_keeps_public_product_pages() {
        let ok = [
            (
                "https://www.rtings.com/tv/reviews/lg/c1-oled?utm_source=x#top",
                "https://rtings.com/tv/reviews/lg/c1-oled",
            ),
            ("HTTP://WWW.Example.COM/a/b/", "http://example.com/a/b/"),
            (
                "https://www.trendyol.com/apple/iphone-15-128-gb-siyah-p-123456789?boutiqueId=1",
                "https://trendyol.com/apple/iphone-15-128-gb-siyah-p-123456789",
            ),
            (
                "https://www.hepsiburada.com/apple-iphone-15-pm-HBC00001234567",
                "https://hepsiburada.com/apple-iphone-15-pm-HBC00001234567",
            ),
            (
                "https://www.tomshardware.com/news/samsung-galaxy-s24-ultra-unboxing-and-firstimpressions-2024",
                "https://tomshardware.com/news/samsung-galaxy-s24-ultra-unboxing-and-firstimpressions-2024",
            ),
            (
                "https://support.apple.com/en-us/HT201295",
                "https://support.apple.com/en-us/HT201295",
            ),
            (
                "https://xn--bcher-kva.com.tr/urun",
                "https://xn--bcher-kva.com.tr/urun",
            ),
            (
                "https://www.ifixit.com/Guide/iPhone+15+Battery+Replacement/165378",
                "https://ifixit.com/Guide/iPhone+15+Battery+Replacement/165378",
            ),
            (
                "https://example.com/manuals/userguide",
                "https://example.com/manuals/userguide",
            ),
        ];
        for (raw, want) in ok {
            assert_eq!(sanitize_url(raw).as_deref(), Some(want), "{raw}");
        }
    }

    #[test]
    fn sanitizer_rejects_personal_order_and_private_urls() {
        let bad = [
            "ftp://example.com/file",
            "mailto:someone@example.com",
            "javascript:alert(1)",
            "https://127.0.0.1/review",
            "http://[::1]/review",
            "http://2130706433/review",
            "http://localhost/review",
            "http://localhost:8000/review",
            "https://printer.local/status",
            "https://wiki.corp.internal/page",
            "https://intranet/page",
            "https://example.com:8443/review",
            "https://user:pw@example.com/review",
            "https://www.trendyol.com/hesabim/siparislerim",
            "https://www.trendyol.com/sepet",
            "https://www.hepsiburada.com/siparis/12345",
            "https://www.hepsiburada.com/uyelik/giris",
            "https://shop.example.com/account/settings",
            "https://shop.example.com/accounts/42",
            "https://shop.example.com/profile",
            "https://shop.example.com/user/ali",
            "https://shop.example.com/users/ali/reviews",
            "https://shop.example.com/member/home",
            "https://shop.example.com/login",
            "https://shop.example.com/login.php",
            "https://shop.example.com/signin",
            "https://shop.example.com/signup",
            "https://shop.example.com/cart",
            "https://shop.example.com/basket",
            "https://shop.example.com/checkout/step-2",
            "https://shop.example.com/order/123",
            "https://shop.example.com/orders/123",
            "https://shop.example.com/hesap",
            "https://shop.example.com/uye/profil",
            "https://shop.example.com/odeme",
            "https://shop.example.com/payment/confirm",
            "https://shop.example.com/invoice/2026-11",
            "https://shop.example.com/fatura/indir",
            "https://cargo.example.com/track/ABC",
            "https://cargo.example.com/tracking",
            "https://news.example.com/unsubscribe",
            "https://shop.example.com/password/reset",
            "https://shop.example.com/token/abc",
            "https://shop.example.com/session/abc",
            "https://shop.example.com/my-orders",
            "https://shop.example.com/me/settings",
            "https://shop.example.com/people/ali@example.com/reviews",
            "https://shop.example.com/people/ali%40example.com",
            "https://shop.example.com/r/eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.sig",
            "https://shop.example.com/r/0123456789abcdef0123456789abcdef",
            "https://shop.example.com/o/550e8400-e29b-41d4-a716-446655440000",
            "https://shop.example.com/s/AbC9dEf8GhI7jKl6MnO5pQr4StU3vWx2",
        ];
        for raw in bad {
            assert_eq!(sanitize_url(raw), None, "{raw}");
        }
        let long = format!("https://example.com/{}", "review-".repeat(50));
        assert_eq!(sanitize_url(&long), None);
    }

    #[test]
    fn product_key_folds_and_rejects_personal_text() {
        assert_eq!(
            product_key("  Sony   WH-1000XM5 ").as_deref(),
            Some("sony wh-1000xm5")
        );
        assert_eq!(
            product_key("Bulaşık Makinesi Şikâyet").as_deref(),
            Some("bulasik makinesi sikayet")
        );
        assert_eq!(
            product_key("İPHONE 15 Pro, ışık").as_deref(),
            Some("iphone 15 pro isik")
        );
        assert_eq!(product_key("Galaxy S24+").as_deref(), Some("galaxy s24+"));
        assert_eq!(
            product_key("ean 8690000000001").as_deref(),
            Some("ean 8690000000001")
        );
        for bad in [
            "ab",
            "a b c d e f g h i",
            "ali@example.com phone",
            "call +90 532 123 45 67",
            "TR33 0006 1005 1978 6457 8413 26",
            "https://rtings.com/x",
            "see http stuff",
            &"x".repeat(121),
        ] {
            assert_eq!(product_key(bad), None, "{bad}");
        }
    }

    #[test]
    fn ingest_rejects_identifying_fields_in_both_shapes() {
        let (s, dir) = store();
        let today = day("2026-11-27");
        let base = json!({
            "product": "sony wh-1000xm5", "kind": "review", "lang": "en", "country": "",
            "url": "https://www.rtings.com/headphones/reviews/sony/wh-1000xm5",
            "signal": "useful", "count": 3, "day": "2026-11-27",
        });
        let corpus = json!({
            "product": "sony wh-1000xm5", "kind": "review", "language": "en", "country": "TR",
            "outcome": "hit", "count": 3, "lastSeen": "2026-11-27T10:00:00Z",
            "topUrls": [{"url": "https://www.rtings.com/headphones/reviews/sony/wh-1000xm5",
                         "domain": "rtings.com", "title": "WH-1000XM5"}],
        });
        let mut lines = Vec::new();
        for field in [
            "user_id", "device", "ip", "email", "session", "userId", "deviceId",
        ] {
            for shape in [&base, &corpus] {
                let mut v = shape.clone();
                v[field] = json!("x");
                lines.push(v.to_string());
            }
        }
        let mut nested = corpus.clone();
        nested["topUrls"][0]["ip"] = json!("10.0.0.1");
        lines.push(nested.to_string());
        let report = s.ingest(&lines.join("\n"), today).unwrap();
        assert_eq!(report["accepted"], 0);
        assert_eq!(report["rejected"], lines.len());
        assert_eq!(s.stats()["urls"], 0);
        let conn = s.lock();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM learned_urls", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ingest_validates_and_maps_corpus_rows_to_served() {
        let (s, dir) = store();
        let today = day("2026-11-27");
        let corpus = json!({
            "product": "Vaillant ecoTEC plus", "kind": "problems", "language": "tr",
            "country": "tr", "outcome": "hit", "count": 4, "lastSeen": "2026-11-27",
            "topUrls": [
                {"url": "https://www.vaillant.com.tr/f28?x=1", "domain": "vaillant.com.tr"},
                {"url": "https://www.vaillant.com.tr/f28#a", "domain": "vaillant.com.tr"},
                {"url": "https://shop.example.com/cart", "domain": "example.com"},
            ],
        });
        let miss = json!({
            "product": "lg c1", "kind": "review", "language": "en", "country": "",
            "outcome": "miss", "count": 5, "lastSeen": "2026-11-26",
            "topUrls": [{"url": "https://www.rtings.com/tv/reviews/lg/c1-oled", "domain": "rtings.com"}],
        });
        let lines = [
            corpus.to_string(),
            miss.to_string(),
            item("lg c1", "https://rtings.com/x", "useful", 0, "2026-11-27"),
            item(
                "lg c1",
                "https://rtings.com/x",
                "useful",
                1001,
                "2026-11-27",
            ),
            item("lg c1", "https://rtings.com/x", "useful", 1, "2026-11-29"),
            item("lg c1", "https://rtings.com/x", "useful", 1, "2025-01-01"),
            item("lg c1", "https://rtings.com/x", "useful", 1, "27/11/2026"),
            item("lg c1", "https://rtings.com/x", "liked", 1, "2026-11-27"),
            item(
                "lg c1",
                "https://rtings.com/account",
                "useful",
                1,
                "2026-11-27",
            ),
            "{not json".to_string(),
        ];
        let report = s.ingest(&lines.join("\n"), today).unwrap();
        assert_eq!(report["accepted"], 2, "{report}");
        assert_eq!(report["rejected"], 8);
        assert!(!report.to_string().contains("rtings.com/account"));

        let conn = s.lock();
        let rows: Vec<(String, String, String, f64, f64, f64)> = conn
            .prepare("SELECT product_key, url, country, useful, unhelpful, served FROM learned_urls ORDER BY url")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
            .unwrap()
            .flatten()
            .collect();
        drop(conn);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "lg c1");
        assert_eq!((rows[0].3, rows[0].4), (0.0, 0.0));
        assert!(rows[0].5 > 4.9 && rows[0].5 < 5.0);
        assert_eq!(rows[1].0, "vaillant ecotec plus");
        assert_eq!(rows[1].1, "https://vaillant.com.tr/f28");
        assert_eq!(rows[1].2, "TR");
        assert_eq!((rows[1].3, rows[1].4, rows[1].5), (0.0, 0.0, 4.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rows_below_k_stay_invisible() {
        let (s, dir) = store();
        let today = day("2026-11-27");
        let lines = [
            item(
                "acme widget",
                "https://acme-widgets.com/widget-review",
                "useful",
                3,
                "2026-11-27",
            ),
            item(
                "acme widget",
                "https://acme-widgets.com/rare-page",
                "useful",
                2,
                "2026-11-27",
            ),
            item(
                "rare gadget",
                "https://rare-gadgets.com/only",
                "useful",
                1,
                "2026-11-27",
            ),
        ];
        s.ingest(&lines.join("\n"), today).unwrap();

        let found: Vec<String> = s
            .for_query("acme widget review")
            .into_iter()
            .map(|l| l.url)
            .collect();
        assert_eq!(found, vec!["https://acme-widgets.com/widget-review"]);
        assert!(s.for_query("rare gadget").is_empty());
        let (boosts, inject) = s.boosts("acme widget");
        assert_eq!(inject.len(), 1);
        assert!(boosts.factor("https://rare-gadgets.com/only") == 1.0);

        let export = s.export().join("\n");
        assert!(export.contains("widget-review"));
        assert!(!export.contains("rare-page") && !export.contains("rare-gadgets"));

        let stats = s.stats();
        assert_eq!(stats["urls"], 1);
        assert_eq!(stats["products"], 1);
        assert_eq!(stats["domains"], 1);
        assert!(!stats.to_string().contains("rare-gadgets"));

        let reimport = s.ingest(&export, today).unwrap();
        assert_eq!(reimport["accepted"], 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retention_and_decay_fade_old_signals() {
        let (s, dir) = store();
        let t0 = day("2026-06-01");
        s.ingest(
            &[
                item(
                    "acme widget",
                    "https://acme-widgets.com/a",
                    "useful",
                    8,
                    "2026-06-01",
                ),
                item(
                    "acme widget",
                    "https://acme-widgets.com/b",
                    "useful",
                    8,
                    "2026-05-02",
                ),
            ]
            .join("\n"),
            t0,
        )
        .unwrap();
        let counts = |s: &LearnStore| -> Vec<f64> {
            s.lock()
                .prepare("SELECT useful FROM learned_urls ORDER BY url")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .flatten()
                .collect()
        };
        let c = counts(&s);
        assert_eq!(c[0], 8.0);
        assert!((c[1] - 8.0 * 0.5f64.powf(30.0 / 90.0)).abs() < 1e-9);

        let r = s.prune(t0 + Duration::days(90));
        assert_eq!(r["decay_days"], 90);
        let c = counts(&s);
        assert!((c[0] - 4.0).abs() < 1e-9, "{c:?}");
        assert_eq!(s.for_query("acme widget").len(), 2);

        let r = s.prune(t0 + Duration::days(200));
        assert_eq!(r["urls_expired"], 2);
        assert_eq!(r["domains_expired"], 1);
        assert!(s.for_query("acme widget").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn served_self_signals_only_touch_domain_stats() {
        let (s, dir) = store();
        let today = day("2026-11-27");
        let domains: HashSet<String> = ["rtings.com", "localhost", "10.0.0.1", "www.ifixit.com"]
            .iter()
            .map(|d| d.to_string())
            .collect();
        for _ in 0..3 {
            s.record_served(&domains, today);
        }
        let stats = s.stats();
        assert_eq!(stats["domains"], 2);
        assert_eq!(stats["urls"], 0);
        assert_eq!(stats["top_domains"][0]["served"], 3.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn learned_and_hint_factors_are_bounded() {
        let strong = Counts {
            useful: 1e6,
            unhelpful: 0.0,
            served: 0.0,
        };
        let weak = Counts {
            useful: 0.0,
            unhelpful: 1e6,
            served: 0.0,
        };
        let neutral = Counts {
            useful: 0.0,
            unhelpful: 0.0,
            served: 50.0,
        };
        assert!((strong.multiplier() - 1.25).abs() < 1e-3);
        assert!((weak.multiplier() - 0.8).abs() < 1e-3);
        assert_eq!(neutral.multiplier(), 1.0);
        let b = Boosts {
            domains: HashMap::from([
                ("good.com".to_string(), strong),
                ("bad.com".to_string(), weak),
            ]),
            urls: HashMap::from([
                ("https://good.com/a".to_string(), strong),
                ("https://bad.com/a".to_string(), weak),
            ]),
        };
        assert_eq!(b.factor("https://www.good.com/a?x=1"), 1.4);
        assert_eq!(b.factor("https://bad.com/a"), 0.75);
        assert_eq!(b.factor("https://other.com/a"), 1.0);

        let many: Vec<String> = (0..21).map(|i| format!("d{i}.com")).collect();
        assert!(Hints::new(&many, &[], None).is_err());
        assert!(Hints::new(&[], &many, None).is_err());
        assert!(Hints::new(&[], &[], Some("english")).is_err());
        let h = Hints::new(
            &["www.RTINGS.com".into(), "localhost".into(), "a/b".into()],
            &["ifixit.com".into()],
            Some("EN"),
        )
        .unwrap();
        assert_eq!(h.prefer.len(), 1);
        assert_eq!(h.factor("rtings.com", Some("en")), 1.3);
        assert_eq!(h.factor("rtings.com", Some("tr")), 1.2);
        assert_eq!(h.factor("ifixit.com", None), 0.7);
        assert_eq!(h.factor("other.com", None), 1.0);
        assert!(Hints::default().is_empty() && !h.is_empty());
    }
}
