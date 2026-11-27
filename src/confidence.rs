use std::collections::HashSet;
use std::sync::OnceLock;

use serde_json::{Value, json};

use crate::discover::{BRAND_TERMS, generic_set};
use crate::extract::SOFT_MIN_CHARS;
use crate::text::{
    is_model_token, match_tokens, product_aliases, query_terms, stem_token, tokenize,
};

const BODY_CHARS: usize = 4000;
const HEAD_CHARS: usize = 500;
pub const MIN_COVERAGE: f64 = 0.66;

struct Concept {
    syns: Vec<String>,
    identity: bool,
}

fn generic_stems() -> &'static HashSet<String> {
    static SET: OnceLock<HashSet<String>> = OnceLock::new();
    SET.get_or_init(|| generic_set().iter().map(|g| stem_token(g)).collect())
}

fn is_identity(term: &str) -> bool {
    is_model_token(term)
        || (term.len() <= 4 && term.bytes().all(|b| b.is_ascii_digit()))
        || BRAND_TERMS.contains(&term)
}

fn concepts(query: &str) -> Vec<Concept> {
    let raw: HashSet<String> = tokenize(query).into_iter().collect();
    let mut out: Vec<Concept> = Vec::new();
    for t in query_terms(query, false) {
        let stem = stem_token(&t);
        if !raw.contains(&t)
            || generic_set().contains(t.as_str())
            || generic_stems().contains(&stem)
            || out.iter().any(|c| c.syns[0] == stem)
        {
            continue;
        }
        let mut syns = vec![stem.clone()];
        for a in product_aliases(&stem) {
            if !syns.contains(&a) {
                syns.push(a);
            }
        }
        out.push(Concept {
            syns,
            identity: is_identity(&t),
        });
    }
    out
}

fn usable(r: &Value, fetched: bool) -> bool {
    if !fetched {
        return true;
    }
    r["status"].as_u64() == Some(200)
        && r["clean_markdown"].as_str().unwrap_or("").chars().count() >= SOFT_MIN_CHARS
}

fn tokens(r: &Value, body_chars: usize) -> HashSet<String> {
    let body: String = r["clean_markdown"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(body_chars)
        .collect();
    match_tokens(&format!(
        "{} {} {} {}",
        r["url"].as_str().unwrap_or(""),
        r["title"].as_str().unwrap_or(""),
        r["snippet"].as_str().unwrap_or(""),
        body
    ))
}

/// Scores whether a result list answers the query well enough to skip a paid fallback.
///
/// Query concepts are the query terms minus stopwords and generic intent words (review, recall,
/// inceleme, ...), stemmed, each with its TR/EN product aliases. Identity concepts are brands,
/// model codes (`c1`, `f28`, `1000xm5`) and bare numbers (`iphone 18`).
///
/// A result is usable when it was fetched with HTTP 200 and >= 300 extracted chars (any result
/// when `fetch=false`). For each usable result, `identity` holds when every identity concept is
/// in its URL, title, snippet or first 500 extracted chars, and `coverage` is the share of all
/// concepts found in those fields plus the first 4000 extracted chars. A result matches when
/// identity holds and coverage >= `MIN_COVERAGE`.
///
/// `confidence = best_coverage * min(1, (matches + 1) / 3)` over identity-matching results, so
/// one full match scores 0.67 and two or more score their coverage. The response is
/// `sufficient` when at least one result matches.
pub fn assess(query: &str, results: &[Value], fetched: bool) -> Value {
    let concepts = concepts(query);
    let mut usable_n = 0usize;
    let mut matches = 0usize;
    let mut best_cov = 0.0f64;
    let mut identity_seen = false;
    for r in results.iter().filter(|r| usable(r, fetched)) {
        usable_n += 1;
        let head = tokens(r, HEAD_CHARS);
        if !concepts
            .iter()
            .filter(|c| c.identity)
            .all(|c| c.syns.iter().any(|s| head.contains(s)))
        {
            continue;
        }
        identity_seen = true;
        let full = tokens(r, BODY_CHARS);
        let cov = if concepts.is_empty() {
            1.0
        } else {
            let hit = concepts
                .iter()
                .filter(|c| c.syns.iter().any(|s| full.contains(s)))
                .count();
            hit as f64 / concepts.len() as f64
        };
        best_cov = best_cov.max(cov);
        if cov >= MIN_COVERAGE {
            matches += 1;
        }
    }
    let support = ((matches + 1) as f64 / 3.0).min(1.0);
    let confidence = (best_cov * support * 1000.0).round() / 1000.0;
    let sufficient = matches > 0;
    let reason = if sufficient {
        None
    } else if results.is_empty() {
        Some("no results")
    } else if usable_n == 0 {
        Some("no result could be fetched and extracted")
    } else if !identity_seen {
        Some("no result names the brand, model or number in the query")
    } else {
        Some("results only partly cover the query terms")
    };
    let identity: Vec<&str> = concepts
        .iter()
        .filter(|c| c.identity)
        .map(|c| c.syns[0].as_str())
        .collect();
    json!({
        "sufficient": sufficient,
        "confidence": confidence,
        "reason": reason,
        "confidence_signals": {
            "results": results.len(),
            "usable": usable_n,
            "matching": matches,
            "best_coverage": (best_cov * 1000.0).round() / 1000.0,
            "concepts": concepts.len(),
            "identity_terms": identity,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(url: &str, title: &str, body: &str) -> Value {
        json!({
            "url": url,
            "title": title,
            "snippet": "",
            "status": 200,
            "clean_markdown": format!("{body} {}", "filler text ".repeat(40)),
        })
    }

    #[test]
    fn empty_or_blocked_results_are_insufficient() {
        let none = assess("lg c1 burn in", &[], true);
        assert_eq!(none["sufficient"], false);
        assert_eq!(none["reason"], "no results");

        let blocked = json!({"url": "https://x.test/lg-c1", "title": "LG C1", "status": 403});
        let v = assess("lg c1 burn in", &[blocked], true);
        assert_eq!(v["sufficient"], false);
        assert_eq!(v["reason"], "no result could be fetched and extracted");
    }

    #[test]
    fn identity_must_match_in_head_not_deep_body() {
        let c1 = page(
            "https://www.rtings.com/tv/reviews/lg/c1-oled",
            "LG C1 OLED review",
            "Burn in risk on the C1 is low.",
        );
        let v = assess("lg c2 burn in", std::slice::from_ref(&c1), true);
        assert_eq!(v["sufficient"], false);
        assert_eq!(
            v["reason"],
            "no result names the brand, model or number in the query"
        );
        assert_eq!(
            v["confidence_signals"]["identity_terms"],
            json!(["lg", "c2"])
        );

        let mut deep = c1.clone();
        deep["clean_markdown"] = json!(format!("{} compared with the C2", "x ".repeat(600)));
        assert_eq!(assess("lg c2 burn in", &[deep], true)["sufficient"], false);
    }

    #[test]
    fn confidence_grows_with_matching_results() {
        let a = page(
            "https://a.test/lg-c1-burn-in",
            "LG C1 burn in test",
            "Burn in after a year.",
        );
        let b = page(
            "https://b.test/c1",
            "LG C1 long term burn in",
            "No burn in so far.",
        );
        let one = assess("lg c1 burn in", std::slice::from_ref(&a), true);
        assert_eq!(one["sufficient"], true);
        assert_eq!(one["confidence"], 0.667);
        assert!(one["reason"].is_null());
        let two = assess("lg c1 burn in", &[a, b], true);
        assert_eq!(two["confidence"], 1.0);
        assert_eq!(two["confidence_signals"]["matching"], 2);
    }

    #[test]
    fn partial_coverage_and_unfetched_results() {
        let door = page(
            "https://a.test/samsung-refrigerator-door",
            "Samsung refrigerator door seal",
            "The door gasket wears out.",
        );
        let v = assess("samsung refrigerator ice maker problem", &[door], true);
        assert_eq!(v["sufficient"], false);
        assert_eq!(v["reason"], "results only partly cover the query terms");
        assert_eq!(v["confidence_signals"]["best_coverage"], 0.5);

        let f28 = page(
            "https://www.vaillant.co.uk/advice/understanding-an-f28-fault-code-on-a-vaillant-boiler",
            "Understanding an F28 fault code on a Vaillant boiler",
            "F28 means ignition failure.",
        );
        assert_eq!(
            assess("vaillant ecotec fault code f28", &[f28], true)["sufficient"],
            true
        );

        let meta = json!({
            "url": "https://a.test/samsung-refrigerator-ice-maker",
            "title": "Samsung refrigerator ice maker not working",
            "status": "not-fetched",
        });
        assert_eq!(
            assess(
                "samsung refrigerator ice maker problem",
                std::slice::from_ref(&meta),
                false
            )["sufficient"],
            true
        );
        assert_eq!(
            assess("samsung refrigerator ice maker problem", &[meta], true)["sufficient"],
            false
        );
    }
}
