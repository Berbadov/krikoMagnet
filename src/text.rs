use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use regex::Regex;
use sha2::{Digest, Sha256};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "best", "by", "can", "do", "does", "for", "from",
    "how", "i", "in", "is", "it", "my", "of", "on", "or", "review", "reviews", "the", "to", "vs",
    "what", "when", "where", "which", "with", "test", "price", "fiyat", "nasil", "nedir", "mi",
    "mu", "bir", "ve", "ile", "icin", "en", "iyi", "hangi", "ne", "2023", "2024", "2025", "2026",
    "not", "after", "before",
];

const PRODUCT_GROUPS: &[&[&str]] = &[
    &["buzdolabi", "refrigerator", "fridge"],
    &["camasir", "washing", "washer"],
    &["bulasik", "dishwasher"],
    &["firin", "oven"],
    &["supurge", "vacuum"],
    &["kombi", "boiler"],
    &["araba", "arac", "car", "otomobil", "vehicle"],
    &["telefon", "phone", "smartphone"],
    &["televizyon", "tv"],
    &["kulaklik", "headphone", "headphones", "earbuds"],
    &["ekran", "screen", "display"],
    &["batarya", "battery"],
    &["klima", "air conditioner"],
    &["termosifon", "water heater"],
    &["recall", "recalled", "cagirma", "toplatma"],
    &["complaint", "complaints", "sikayet", "sorun"],
    &["review", "inceleme", "yorum"],
    &["price", "fiyat"],
    &["repair", "tamir", "onarim"],
    &["fault", "ariza", "hata", "error"],
    &["pressure", "basinc"],
    &["leak", "akitiyor", "akma"],
    &["drain", "tahliye", "bosaltma"],
];

fn stopwords() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| STOPWORDS.iter().copied().collect())
}

fn alias_map() -> &'static HashMap<String, Vec<String>> {
    static MAP: OnceLock<HashMap<String, Vec<String>>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut idx = HashMap::new();
        for group in PRODUCT_GROUPS {
            let mut aliases: Vec<String> = group.iter().map(|a| stem_token(a)).collect();
            aliases.sort();
            aliases.dedup();
            for &member in *group {
                idx.insert(member.to_string(), aliases.clone());
                idx.insert(stem_token(member), aliases.clone());
            }
        }
        idx
    })
}

pub fn fold(text: &str) -> String {
    let mapped: String = text
        .chars()
        .map(|c| match c {
            'ç' | 'Ç' => 'c',
            'ğ' | 'Ğ' => 'g',
            'ı' | 'İ' | 'î' => 'i',
            'ö' | 'Ö' => 'o',
            'ş' | 'Ş' => 's',
            'ü' | 'Ü' | 'û' => 'u',
            'â' => 'a',
            _ => c,
        })
        .collect();
    mapped
        .nfkd()
        .filter(|&c| !is_combining_mark(c))
        .flat_map(|c| c.to_lowercase())
        .collect()
}

pub fn tokenize(text: &str) -> Vec<String> {
    let folded = fold(text);
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in folded.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            cur.push(ch);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn is_model_token(token: &str) -> bool {
    let len = token.len();
    (2..=8).contains(&len)
        && token
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && token.bytes().any(|b| b.is_ascii_lowercase())
        && token.bytes().any(|b| b.is_ascii_digit())
}

pub fn query_terms(query: &str, keep_stopwords: bool) -> Vec<String> {
    let raw = tokenize(query);
    let sw = stopwords();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for token in &raw {
        if token.len() < 2 && !token.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if !keep_stopwords && sw.contains(token.as_str()) {
            continue;
        }
        if seen.insert(token.clone()) {
            out.push(token.clone());
        }
    }
    for pair in raw.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let combined = format!("{a}{b}");
        if (3..=20).contains(&combined.len()) && a.len() <= 3 && b.len() <= 3 {
            if !keep_stopwords && (sw.contains(a.as_str()) || sw.contains(b.as_str())) {
                continue;
            }
            if seen.insert(combined.clone()) {
                out.push(combined);
            }
        }
    }
    for pair in raw.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let a_alpha = a.bytes().all(|c| c.is_ascii_alphabetic());
        let a_digit = a.bytes().all(|c| c.is_ascii_digit());
        let b_alpha = b.bytes().all(|c| c.is_ascii_alphabetic());
        let b_digit = b.bytes().all(|c| c.is_ascii_digit());
        if (a_alpha && b_digit) || (a_digit && b_alpha) {
            let combined = format!("{a}{b}");
            if (3..=12).contains(&combined.len()) && seen.insert(combined.clone()) {
                out.push(combined);
            }
        }
    }
    out
}

pub fn slugify(text: &str) -> String {
    query_terms(text, true).join("-")
}

fn stem_once(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    let len = chars.len();
    if len <= 3 || token.bytes().all(|b| b.is_ascii_digit()) {
        return token.to_string();
    }
    let slice = |n: usize| -> String { chars[..len - n].iter().collect() };
    if token.ends_with("leri") || token.ends_with("lari") || token.ends_with("ları") {
        return slice(4);
    }
    if (token.ends_with("miyor")
        || token.ends_with("mıyor")
        || token.ends_with("muyor")
        || token.ends_with("müyor"))
        && len > 6
    {
        return slice(5);
    }
    if (token.ends_with("uyor")
        || token.ends_with("üyor")
        || token.ends_with("ıyor")
        || token.ends_with("iyor"))
        && len > 6
    {
        return slice(4);
    }
    if (token.ends_with("maz") || token.ends_with("mez")) && len > 5 {
        return slice(3);
    }
    if (token.ends_with("er") || token.ends_with("ar")) && len >= 5 {
        return slice(2);
    }
    if token.ends_with("ment") && len > 6 {
        return slice(4);
    }
    if token.ends_with("ies") && len > 4 {
        return format!("{}y", slice(3));
    }
    if token.ends_with("ied") && len > 4 {
        return format!("{}y", slice(3));
    }
    if token.ends_with("ing") && len > 5 {
        return slice(3);
    }
    if token.ends_with("ed") && len > 4 {
        return slice(2);
    }
    if token.ends_with("es") && len > 4 {
        return slice(1);
    }
    if token.ends_with('s') && !token.ends_with("ss") && len > 3 {
        return slice(1);
    }
    if token.ends_with("si") && len > 5 {
        return slice(2);
    }
    if (token.ends_with("yu") || token.ends_with("yü")) && len > 4 {
        return slice(2);
    }
    if (token.ends_with("ma") || token.ends_with("me")) && len > 5 {
        return slice(2);
    }
    if let Some(&last) = chars.last()
        && matches!(last, 'i' | 'ı' | 'u' | 'ü')
        && len > 5
    {
        return slice(1);
    }
    token.to_string()
}

pub fn stem_token(token: &str) -> String {
    let mut tok = fold(token);
    for _ in 0..3 {
        let next = stem_once(&tok);
        if next == tok {
            break;
        }
        tok = next;
    }
    tok
}

pub fn match_tokens(text: &str) -> HashSet<String> {
    tokenize(text).into_iter().map(|t| stem_token(&t)).collect()
}

pub fn product_aliases(term: &str) -> Vec<String> {
    let map = alias_map();
    map.get(term)
        .cloned()
        .unwrap_or_else(|| vec![stem_token(term)])
}

pub fn ranker_text(text: &str) -> String {
    tokenize(text)
        .into_iter()
        .map(|t| stem_token(&t))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn ranker_query(text: &str) -> String {
    let terms = query_terms(text, false);
    let term_set: HashSet<&str> = terms.iter().map(|s| s.as_str()).collect();
    let mut extra = Vec::new();
    for term in &terms {
        for alias in product_aliases(term) {
            if !term_set.contains(alias.as_str()) && !extra.contains(&alias) {
                extra.push(alias);
            }
        }
    }
    extra.sort();
    ranker_text(&format!("{text} {}", extra.join(" ")))
}

pub fn unslug(url: &str) -> String {
    static EXT_RE: OnceLock<Regex> = OnceLock::new();
    static SEP_RE: OnceLock<Regex> = OnceLock::new();
    let ext_re = EXT_RE.get_or_init(|| Regex::new(r"(?i)\.(html?|php\d?|aspx?|jsp)$").unwrap());
    let sep_re = SEP_RE.get_or_init(|| Regex::new(r"[-_/]+").unwrap());

    let after_scheme = url.split_once("://").map(|x| x.1).unwrap_or(url);
    let path = after_scheme.split_once('/').map(|x| x.1).unwrap_or("");
    let path = path.split(&['?', '#'][..]).next().unwrap_or("");
    let path = ext_re.replace(path, "");
    sep_re
        .replace_all(&path, " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn long_ago(days: i64) -> String {
    (Utc::now() - Duration::days(days)).to_rfc3339()
}

pub fn iso_date(value: Option<&str>) -> Option<String> {
    let v = value?.trim();
    if v.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(v) {
        return Some(dt.with_timezone(&Utc).to_rfc3339());
    }
    if let Ok(dt) = DateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S%z") {
        return Some(dt.with_timezone(&Utc).to_rfc3339());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%d %H:%M:%S") {
        return Some(DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc).to_rfc3339());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S") {
        return Some(DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc).to_rfc3339());
    }
    if let Ok(dt) = DateTime::parse_from_rfc2822(v) {
        return Some(dt.with_timezone(&Utc).to_rfc3339());
    }
    let prefix: String = v.chars().take(10).collect();
    if let Ok(d) = NaiveDate::parse_from_str(&prefix, "%Y-%m-%d") {
        let dt = d.and_hms_opt(0, 0, 0)?;
        return Some(DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc).to_rfc3339());
    }
    None
}

pub fn sha16(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut out = String::with_capacity(16);
    for b in &digest[..8] {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_compaction_and_turkish_stemming() {
        let terms = query_terms("samsung galaxy note 7 recall", false);
        assert!(terms.contains(&"note7".to_string()));
        assert_eq!(stem_token("filtresi"), "filtre");
        assert_eq!(stem_token("arizasi"), "ariza");
        assert_eq!(stem_token("recalled"), "recall");
        assert_eq!(stem_token("dishwashers"), "dishwash");
    }

    #[test]
    fn fold_and_unslug() {
        assert_eq!(fold("Bulaşık Makinesi Şikâyet"), "bulasik makinesi sikayet");
        assert_eq!(
            unslug("https://www.rtings.com/tv/reviews/lg/c1-oled.html?foo=1"),
            "tv reviews lg c1 oled"
        );
    }
}
