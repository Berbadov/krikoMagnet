use std::collections::HashMap;

use chrono::{DateTime, Utc};

const K1: f64 = 1.2;
const B: f64 = 0.75;
const PRIOR_LN: f64 = 13.815510557964274; // ln(1_000_000)
const TAU_DAYS: f64 = 90.0;
const ALPHA: f64 = 0.40;
const BETA: f64 = 0.60;
const GAMMA: f64 = 0.15;
const DELTA: f64 = 0.30;

#[derive(Clone, Debug)]
pub struct RankInput {
    pub id: String,
    pub domain: String,
    pub title: String,
    pub snippet: String,
    pub published: String,
    pub discovery: f64,
}

#[derive(Clone, Debug)]
pub struct RankedDoc {
    pub id: String,
    pub score: f64,
}

fn tokenize_ascii(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.to_lowercase().chars() {
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

fn parse_epoch(s: &str, fallback: f64) -> f64 {
    DateTime::parse_from_rfc3339(s)
        .map(|d| {
            let utc = d.with_timezone(&Utc);
            utc.timestamp() as f64 + utc.timestamp_subsec_micros() as f64 / 1e6
        })
        .unwrap_or(fallback)
}

pub fn rank_docs(
    query_text: &str,
    docs: &[RankInput],
    domain_rank: &HashMap<String, u32>,
    now_epoch: f64,
) -> Vec<RankedDoc> {
    if docs.is_empty() {
        return Vec::new();
    }
    let mut ql = tokenize_ascii(query_text);
    ql.sort();
    ql.dedup();
    if ql.is_empty() {
        return docs
            .iter()
            .map(|d| RankedDoc {
                id: d.id.clone(),
                score: 0.0,
            })
            .collect();
    }

    let n = docs.len();
    let mut doc_tokens = Vec::with_capacity(n);
    let mut df: HashMap<String, u32> = HashMap::new();
    let mut total_len = 0usize;

    for doc in docs {
        let toks = tokenize_ascii(&format!("{} {}", doc.title, doc.snippet));
        total_len += toks.len();
        let mut seen: Vec<&str> = Vec::with_capacity(toks.len().min(64));
        for t in &toks {
            if !seen.contains(&t.as_str()) {
                seen.push(t.as_str());
            }
        }
        for t in seen {
            *df.entry(t.to_string()).or_insert(0) += 1;
        }
        doc_tokens.push(toks);
    }
    let avgdl = (total_len as f64 / n as f64).max(1e-9);

    let qidx: HashMap<&str, usize> = ql
        .iter()
        .enumerate()
        .map(|(i, t)| (t.as_str(), i))
        .collect();
    let mut freq = vec![0u32; ql.len()];
    let mut bm = vec![0.0f64; n];
    let mut max_bm = 0.0f64;

    for (i, toks) in doc_tokens.iter().enumerate() {
        freq.fill(0);
        // Floor effective length at 0.65 * avgdl so short metadata-only stubs without snippets do not dominate rich abstracts.
        let dl = (toks.len() as f64).max(avgdl * 0.65);
        let mut matched = false;
        for t in toks {
            if let Some(&j) = qidx.get(t.as_str()) {
                freq[j] += 1;
                matched = true;
            }
        }
        if !matched {
            continue;
        }
        let mut bm25 = 0.0;
        for (j, &f) in freq.iter().enumerate() {
            if f == 0 {
                continue;
            }
            let df_t = df.get(&ql[j]).copied().unwrap_or(0) as f64;
            let idf = (1.0 + (n as f64 - df_t + 0.5) / (df_t + 0.5)).ln();
            let denom = f as f64 + K1 * (1.0 - B + B * dl / avgdl);
            bm25 += idf * (f as f64 * (K1 + 1.0)) / denom;
        }
        bm[i] = bm25;
        if bm25 > max_bm {
            max_bm = bm25;
        }
    }
    if max_bm <= 0.0 {
        max_bm = 1.0;
    }

    let mut ranked: Vec<(usize, f64)> = Vec::with_capacity(n);
    for (i, doc) in docs.iter().enumerate() {
        if bm[i] <= 0.0 {
            ranked.push((i, 0.0));
            continue;
        }
        let prior = match domain_rank.get(&doc.domain) {
            Some(&r) => (1_000_000f64 / (r as f64).max(1.0)).ln(),
            None => 0.0,
        };
        let pub_epoch = parse_epoch(&doc.published, now_epoch);
        let days = ((now_epoch - pub_epoch) / 86400.0).max(0.0);
        let fresh = (-days / TAU_DAYS).exp();
        let bm_s = bm[i] / max_bm;
        let prior_s = prior / PRIOR_LN;
        let sc = ALPHA * bm_s + BETA * prior_s + GAMMA * fresh + DELTA * doc.discovery;
        ranked.push((i, sc));
    }
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked
        .into_iter()
        .map(|(i, score)| RankedDoc {
            id: docs[i].id.clone(),
            score,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_lexical_and_authority_match_first() {
        let mut tranco = HashMap::new();
        tranco.insert("rtings.com".to_string(), 1500);
        let docs = vec![
            RankInput {
                id: "a".into(),
                domain: "rtings.com".into(),
                title: "lg c1 oled review".into(),
                snippet: "burn in test".into(),
                published: "2026-11-01T00:00:00Z".into(),
                discovery: 0.8,
            },
            RankInput {
                id: "b".into(),
                domain: "example.com".into(),
                title: "random tv news".into(),
                snippet: "oled panels".into(),
                published: "2026-11-01T00:00:00Z".into(),
                discovery: 0.1,
            },
            RankInput {
                id: "c".into(),
                domain: "rtings.com".into(),
                title: "headphones guide".into(),
                snippet: "wireless audio".into(),
                published: "2026-11-01T00:00:00Z".into(),
                discovery: 0.9,
            },
        ];
        let ranked = rank_docs("lg c1 burn in", &docs, &tranco, 1794000000.0);
        assert_eq!(ranked[0].id, "a");
        assert!(ranked[0].score > ranked[1].score);
        assert_eq!(ranked[2].id, "c");
        assert_eq!(ranked[2].score, 0.0);
    }
}
