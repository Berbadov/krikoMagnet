use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use regex::Regex;
use serde::Deserialize;

use crate::text::{fold, query_terms};

#[derive(Clone, Debug, Deserialize)]
pub struct Method {
    #[serde(rename = "type")]
    pub kind: String,
    pub url: Option<String>,
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub include_children: Vec<String>,
    #[serde(default)]
    pub exclude_children: Vec<String>,
    pub max_sitemaps: Option<usize>,
    pub max_urls: Option<usize>,
    pub max_results: Option<usize>,
    pub probes: Option<usize>,
    pub link_selector: Option<String>,
    pub base_url: Option<String>,
    pub items_path: Option<String>,
    pub title: Option<String>,
    pub snippet: Option<String>,
    pub url_field: Option<String>,
    pub url_template: Option<String>,
    pub body_field: Option<String>,
    pub date: Option<String>,
    pub timeout: Option<f64>,
    #[serde(default)]
    pub templates: Vec<String>,
    pub max_query_terms: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Site {
    pub id: String,
    pub name: String,
    pub domain: String,
    pub topics: Vec<String>,
    pub language: String,
    pub priority: Option<f64>,
    pub timeout_s: Option<f64>,
    pub index_max_age_days: Option<u64>,
    pub index_max_children: Option<usize>,
    pub methods: Vec<Method>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct OpenWebConfig {
    #[serde(default)]
    pub enabled: bool,
    pub index: Option<String>,
    #[serde(default)]
    pub domains: Vec<String>,
    pub max_domains: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SkippedSite {
    pub domain: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Registry {
    pub version: u32,
    pub sites: Vec<Site>,
    pub open_web: Option<OpenWebConfig>,
    #[serde(default)]
    pub skipped: Vec<SkippedSite>,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub language: String,
    pub topics: Vec<String>,
    pub sites: Vec<String>,
    pub fallback: Option<String>,
    pub selected: Vec<Site>,
}

const VALID_METHODS: &[&str] = &[
    "search",
    "sitemap",
    "rss",
    "api",
    "json_search",
    "url_pattern",
];

const TOPIC_TERMS: &[(&str, &[&str])] = &[
    (
        "phones",
        &[
            "iphone",
            "ipad",
            "samsung",
            "galaxy",
            "pixel",
            "xiaomi",
            "redmi",
            "oneplus",
            "oppo",
            "vivo",
            "huawei",
            "honor",
            "realme",
            "nothing",
            "phone",
            "smartphone",
            "telefon",
            "batarya",
            "battery",
            "sarj",
            "charging",
            "camera",
            "kamera",
            "ekran",
            "screen",
            "airpods",
            "anker",
            "powerbank",
            "compatibility",
            "compatible",
            "uyumlu",
            "uyum",
            "lg",
        ],
    ),
    (
        "computing",
        &[
            "laptop",
            "notebook",
            "gpu",
            "rtx",
            "gtx",
            "radeon",
            "cpu",
            "ryzen",
            "intel",
            "ssd",
            "nvme",
            "ram",
            "monitor",
            "gaming",
            "oyun",
            "bilgisayar",
            "islemci",
            "ekran karti",
            "macbook",
            "windows",
            "linux",
            "compatibility",
            "compatible",
            "uyumlu",
            "uyum",
            "anker",
            "powerbank",
            "sarj",
            "usb",
            "thunderbolt",
        ],
    ),
    (
        "tv-audio",
        &[
            "tv",
            "televizyon",
            "oled",
            "qled",
            "soundbar",
            "speaker",
            "hoparlor",
            "kulaklik",
            "headphone",
            "headphones",
            "earbuds",
            "monitor",
            "lg",
            "sony",
            "panasonic",
            "philips",
            "tcl",
            "hisense",
            "c1",
            "c2",
            "g1",
            "g2",
            "a95",
            "qn90",
            "x90",
            "noise",
            "burn",
            "panel",
        ],
    ),
    (
        "appliances",
        &[
            "dishwasher",
            "washing",
            "washer",
            "dryer",
            "fridge",
            "refrigerator",
            "oven",
            "vacuum",
            "robot",
            "bulasik",
            "camasir",
            "kurutma",
            "buzdolabi",
            "firin",
            "supurge",
            "ankastre",
            "ocak",
            "dyson",
            "bosch",
            "arcelik",
            "beko",
            "samsung",
            "lg",
            "whirlpool",
            "maytag",
            "kitchenaid",
            "error",
            "bulasik makinesi",
            "camasir makinesi",
            "kurutma makinesi",
            "derin dondurucu",
            "davlumbaz",
            "su isitici",
            "sogutmuyor",
            "isinmiyor",
            "su akitiyor",
            "calismiyor",
            "bozuk",
            "servis",
            "yedek parca",
        ],
    ),
    (
        "cars",
        &[
            "car",
            "otomobil",
            "araba",
            "arac",
            "renault",
            "megane",
            "clio",
            "captur",
            "ford",
            "toyota",
            "honda",
            "hyundai",
            "kia",
            "bmw",
            "mercedes",
            "volkswagen",
            "vw",
            "audi",
            "skoda",
            "dacia",
            "peugeot",
            "citroen",
            "opel",
            "fiat",
            "dizel",
            "diesel",
            "motor",
            "yag",
            "filtre",
            "lastik",
            "brake",
            "fren",
            "suv",
            "sedan",
            "cr-v",
            "corolla",
            "golf",
            "passat",
            "focus",
            "recall",
            "crv",
            "ariza lambasi",
            "servis bakim",
            "yedek parca",
            "vites",
            "motor yagi",
            "yakit",
            "kasko",
        ],
    ),
    (
        "boilers",
        &[
            "boiler",
            "kombi",
            "vaillant",
            "ecotec",
            "buderus",
            "heating",
            "isitma",
            "radyator",
            "petek",
            "ariza",
            "fault",
            "error",
            "pressure",
            "basinc",
            "termostat",
            "thermostat",
            "radiator",
            "kombi ariza",
            "ariza kodu",
            "isinmiyor",
            "su isitmiyor",
            "baymak",
            "demirdokum",
            "eca",
        ],
    ),
    (
        "recalls",
        &[
            "recall", "recalled", "recalls", "geri", "cagirma", "toplatma", "guvensiz", "unsafe",
            "safety", "cpsc", "rapex", "gubis", "warning", "hazard",
        ],
    ),
    (
        "repair",
        &[
            "repair",
            "replacement",
            "replace",
            "fix",
            "teardown",
            "tamir",
            "degisim",
            "degistirme",
            "parca",
            "spare",
            "battery replacement",
            "screen replacement",
            "kirik",
            "bozuk",
            "servis",
        ],
    ),
    (
        "complaints",
        &[
            "complaint",
            "complaints",
            "problem",
            "issue",
            "issues",
            "sorun",
            "sikayet",
            "ariza",
            "arizali",
            "faulty",
            "warranty",
            "garanti",
            "service",
            "servis",
            "kullanici",
            "yorum",
        ],
    ),
    (
        "reviews",
        &[
            "review",
            "reviews",
            "inceleme",
            "test",
            "yorum",
            "comparison",
            "karsilastirma",
            "best",
            "rated",
            "vs",
            "deneyim",
            "experience",
        ],
    ),
    (
        "apple",
        &[
            "iphone",
            "ipad",
            "macbook",
            "imac",
            "airpods",
            "apple",
            "ios",
            "macos",
            "watch",
            "apple watch",
        ],
    ),
];

const TR_STOPWORDS: &[&str] = &[
    "ve", "bir", "ile", "icin", "mi", "mu", "ne", "nasil", "hangi", "fiyat", "sorun", "sikayet",
    "ariza", "servis", "garanti", "yorum", "inceleme",
];

pub fn load_registry(path: &Path) -> Result<Registry, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let reg: Registry =
        serde_yaml::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    if reg.version == 0 || reg.sites.is_empty() {
        return Err(format!("registry invalid or empty: {}", path.display()));
    }
    let mut seen = HashSet::new();
    for s in &reg.sites {
        if !seen.insert(&s.id) {
            return Err(format!("duplicate site id: {}", s.id));
        }
        if s.topics.is_empty() || s.methods.is_empty() {
            return Err(format!("site {} missing topics/methods", s.id));
        }
        for m in &s.methods {
            if !VALID_METHODS.contains(&m.kind.as_str()) {
                return Err(format!("site {} unknown method {}", s.id, m.kind));
            }
        }
    }
    Ok(reg)
}

pub fn load_tranco(path: &Path) -> HashMap<String, u32> {
    let Ok(txt) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    let mut ranks = HashMap::new();
    for (i, line) in txt.lines().enumerate() {
        let d = line.trim().to_ascii_lowercase();
        if !d.is_empty() {
            ranks.entry(d).or_insert((i + 1) as u32);
        }
    }
    ranks
}

pub fn detect_language(query: &str) -> &'static str {
    if query.chars().any(|c| "çğıİöşüÇĞÖŞÜ".contains(c)) {
        return "tr";
    }
    let folded = fold(query);
    let toks: HashSet<&str> = folded
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();
    if TR_STOPWORDS.iter().any(|sw| toks.contains(sw)) {
        "tr"
    } else {
        "en"
    }
}

pub fn classify(query: &str) -> (String, Vec<String>, Vec<String>) {
    let text = fold(query);
    let tokens: Vec<&str> = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();
    let tok_set: HashSet<&str> = tokens.iter().copied().collect();
    let mut hits: Vec<(&str, usize)> = Vec::new();
    for &(topic, terms) in TOPIC_TERMS {
        let mut count = 0usize;
        for &term in terms {
            if term.contains(' ') {
                if text.contains(term) {
                    count += 1;
                }
            } else if tok_set.contains(term)
                || (term.len() >= 4 && tokens.iter().any(|t| t.starts_with(term)))
            {
                count += 1;
            }
        }
        if count > 0 {
            hits.push((topic, count));
        }
    }
    hits.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let topics = hits.into_iter().map(|(t, _)| t.to_string()).collect();
    (
        detect_language(query).to_string(),
        topics,
        query_terms(query, false),
    )
}

fn prior_score(domain: &str, ranks: &HashMap<String, u32>) -> f64 {
    let Some(&r) = ranks.get(domain) else {
        return 0.0;
    };
    if r == 0 {
        return 0.0;
    }
    (100.0 * (1_000_000.0 / r as f64).ln() / 1_000_000.0_f64.ln()).max(0.0)
}

fn term_in(term: &str, site_terms: &HashSet<&str>) -> bool {
    if site_terms.contains(term) {
        return true;
    }
    if term.len() < 4 {
        return false;
    }
    site_terms
        .iter()
        .any(|&t| t.len() >= 5 && (t.starts_with(term) || (term.len() >= 5 && term.starts_with(t))))
}

fn domain_tokens(domain: &str) -> HashSet<String> {
    domain
        .to_ascii_lowercase()
        .split('.')
        .flat_map(|label| label.split(['-', '_']))
        .filter(|part| part.len() >= 3)
        .map(str::to_string)
        .collect()
}

pub fn is_recall_query(query: &str) -> bool {
    let terms = query_terms(query, false);
    terms.iter().any(|t| {
        matches!(
            t.as_str(),
            "recall" | "recalls" | "recalled" | "guvensiz" | "toplatma" | "cagirma"
        )
    })
}

pub fn plan_sites(
    query: &str,
    reg: &Registry,
    ranks: &HashMap<String, u32>,
    max_sites: usize,
) -> Plan {
    let (language, mut topics_vec, terms) = classify(query);
    let mut fallback = None;
    if topics_vec.is_empty() {
        topics_vec = vec!["general".to_string()];
        fallback = Some("general".to_string());
    }
    let topics: HashSet<&str> = topics_vec.iter().map(|s| s.as_str()).collect();
    let topic_map: HashMap<&str, &[&str]> = TOPIC_TERMS.iter().copied().collect();
    let recall_query = topics.contains("recalls") || is_recall_query(query);

    let mut scored: Vec<(f64, &Site)> = Vec::new();
    for site in &reg.sites {
        if (site.id == "cpsc" || site.id == "guvensizurun") && !recall_query {
            continue;
        }
        let overlap = site
            .topics
            .iter()
            .filter(|t| topics.contains(t.as_str()))
            .count();
        let mut site_terms: HashSet<&str> = HashSet::new();
        for t in &site.topics {
            if let Some(list) = topic_map.get(t.as_str()) {
                site_terms.extend(list.iter().copied());
            }
        }
        let matched = terms
            .iter()
            .filter(|t| term_in(t.as_str(), &site_terms))
            .count();
        if overlap == 0 && matched == 0 {
            continue;
        }
        let lang_bonus = if site.language == language { 1 } else { 0 };
        let dom_toks = domain_tokens(&site.domain);
        let dom_hits = terms
            .iter()
            .filter(|t| t.len() >= 3 && dom_toks.contains(t.as_str()))
            .count();
        let prior = prior_score(&site.domain, ranks);
        let score = matched as f64 * 1000.0
            + overlap as f64 * 100.0
            + lang_bonus as f64 * 300.0
            + dom_hits as f64 * 800.0
            + site.priority.unwrap_or(0.0)
            + prior;
        scored.push((score, site));
    }

    if scored.is_empty() {
        for fallback_topic in ["general", "reviews"] {
            for site in &reg.sites {
                if site.topics.iter().any(|t| t == fallback_topic) {
                    scored.push((prior_score(&site.domain, ranks), site));
                }
            }
            if !scored.is_empty() {
                break;
            }
        }
    }

    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    let selected: Vec<Site> = scored
        .into_iter()
        .take(max_sites)
        .map(|(_, s)| s.clone())
        .collect();
    let sites = selected.iter().map(|s| s.id.clone()).collect();
    let _ = Regex::new(""); // keep consistent imports clean if needed
    Plan {
        language,
        topics: topics_vec,
        sites,
        fallback,
        selected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planner_routes_turkish_and_general_queries() {
        let reg = load_registry(Path::new("registry.yaml")).expect("registry.yaml");
        let ranks = HashMap::new();

        let tr = plan_sites("vaillant kombi arıza kodu f28", &reg, &ranks, 6);
        assert_eq!(tr.language, "tr");
        assert_eq!(tr.sites[0], "vaillanttr");
        assert!(tr.fallback.is_none());

        let bosch = plan_sites("bosch bulaşık makinesi su almıyor", &reg, &ranks, 6);
        assert_eq!(bosch.sites[0], "boschhome");

        let recall = plan_sites("anker power bank recall", &reg, &ranks, 6);
        assert!(recall.sites.contains(&"cpsc".to_string()));

        let non_recall = plan_sites("lg c1 burn in", &reg, &ranks, 6);
        assert!(!non_recall.sites.contains(&"cpsc".to_string()));

        let gen_plan = plan_sites(
            "kolmogorov complexity and related information",
            &reg,
            &ranks,
            6,
        );
        assert_eq!(gen_plan.fallback.as_deref(), Some("general"));
        assert!(gen_plan.sites.contains(&"wikipedia".to_string()));
        assert!(!gen_plan.sites.contains(&"gsmarena".to_string()));
    }
}
