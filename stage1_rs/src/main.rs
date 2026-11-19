// Kriko Magnet Stage 1 hybrid ranker (Rust port of stage1_py/ranker.py).
// Mirrors the Python scorer exactly: BM25(title+snippet) + log-scaled Tranco
// authority prior + exponential freshness, additive over lexically-gated docs.
// Optimized: token arrays are precomputed once at load; per-query ranking only
// does a single pass over tokens with a reusable frequency buffer.
use std::collections::HashMap;
use std::env;
use std::fs;
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde_json::Value;

const K1: f64 = 1.2;
const B: f64 = 0.75;
const PRIOR_LN: f64 = 13.815510557964274; // ln(1_000_000)
const TAU_DAYS: f64 = 90.0;
const ALPHA: f64 = 0.40;
const BETA: f64 = 0.60;
const GAMMA: f64 = 0.15;

struct Doc {
    id: String,
    domain: String,
    published_epoch: f64,
}

struct Corpus {
    docs: Vec<Doc>,
    tokens: Vec<Vec<String>>,
    lens: Vec<usize>,
    df: HashMap<String, u32>,
    avgdl: f64,
}

fn tokenize(s: &str) -> Vec<String> {
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

fn parse_rfc3339_epoch(s: &str) -> f64 {
    let dt: DateTime<Utc> = DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    dt.timestamp() as f64 + dt.timestamp_subsec_micros() as f64 / 1e6
}

fn load_tranco(path: &str) -> HashMap<String, u32> {
    let mut m = HashMap::new();
    let txt = fs::read_to_string(path).expect("read tranco");
    for (i, line) in txt.lines().enumerate() {
        let d = line.trim();
        if !d.is_empty() && !m.contains_key(d) {
            m.insert(d.to_string(), (i + 1) as u32);
        }
    }
    m
}

fn load_corpus(path: &str) -> Corpus {
    let txt = fs::read_to_string(path).expect("read corpus");
    let mut docs = Vec::new();
    let mut tokens = Vec::new();
    let mut lens = Vec::new();
    let mut df: HashMap<String, u32> = HashMap::new();
    let mut total_len: usize = 0;
    for line in txt.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line).expect("corpus json");
        let title = v["title"].as_str().unwrap_or("").to_string();
        let snippet = v["snippet"].as_str().unwrap_or("").to_string();
        let toks = tokenize(&format!("{} {}", title, snippet));
        let tl = toks.len();
        total_len += tl;
        // df over distinct tokens per doc
        let mut seen: Vec<&str> = Vec::with_capacity(tl.min(64));
        for t in &toks {
            if !seen.contains(&t.as_str()) {
                seen.push(t.as_str());
            }
        }
        for t in seen {
            *df.entry(t.to_string()).or_insert(0) += 1;
        }
        docs.push(Doc {
            id: v["id"].as_str().unwrap_or("").to_string(),
            domain: v["domain"].as_str().unwrap_or("").to_string(),
            published_epoch: parse_rfc3339_epoch(v["published"].as_str().unwrap_or("")),
        });
        tokens.push(toks);
        lens.push(tl);
    }
    let n = docs.len();
    let avgdl = if n == 0 { 0.0 } else { total_len as f64 / n as f64 };
    Corpus { docs, tokens, lens, df, avgdl }
}

fn load_queries(path: &str) -> Vec<(String, String)> {
    let txt = fs::read_to_string(path).expect("read queries");
    let v: Value = serde_json::from_str(&txt).expect("queries json");
    let mut q = Vec::new();
    if let Value::Array(arr) = v {
        for item in arr {
            q.push((
                item["id"].as_str().unwrap_or("").to_string(),
                item["text"].as_str().unwrap_or("").to_string(),
            ));
        }
    }
    q
}

fn rank(query_text: &str, c: &Corpus, domain_rank: &HashMap<String, u32>, now: f64) -> Vec<(usize, f64)> {
    let mut ql = tokenize(query_text);
    ql.sort();
    ql.dedup(); // mirror Python's set(qterms)
    let nq = ql.len();
    let n = c.docs.len();

    // term -> index within query
    let qidx: HashMap<&str, usize> = ql.iter().enumerate().map(|(i, t)| (t.as_str(), i)).collect();
    let mut freq = vec![0u32; nq];

    let mut ranked: Vec<(usize, f64)> = Vec::with_capacity(n);
    let mut max_bm: f64 = 0.0;
    let mut bm = vec![0.0f64; n];

    for i in 0..n {
        freq.fill(0);
        let toks = &c.tokens[i];
        let dl = c.lens[i] as f64;
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
        for j in 0..nq {
            let f = freq[j];
            if f == 0 {
                continue;
            }
            let df_t = c.df.get(&ql[j]).cloned().unwrap_or(0) as f64;
            let idf = (1.0 + (n as f64 - df_t + 0.5) / (df_t + 0.5)).ln();
            let denom = f as f64 + K1 * (1.0 - B + B * dl / c.avgdl);
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

    for i in 0..n {
        if bm[i] <= 0.0 {
            ranked.push((i, 0.0));
            continue;
        }
        let prior = match domain_rank.get(&c.docs[i].domain) {
            Some(&r) => (1_000_000f64 / (r as f64).max(1.0)).ln(),
            None => 0.0,
        };
        let days = ((now - c.docs[i].published_epoch) / 86400.0).max(0.0);
        let fresh = (-days / TAU_DAYS).exp();
        let bm_s = bm[i] / max_bm;
        let prior_s = prior / PRIOR_LN;
        let sc = ALPHA * bm_s + BETA * prior_s + GAMMA * fresh;
        ranked.push((i, sc));
    }
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut corpus = String::new();
    let mut queries = String::new();
    let mut tranco = String::new();
    let mut now_opt: Option<f64> = None;
    let mut bench = false;
    let mut scores = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus" => { corpus = args[i + 1].clone(); i += 2; }
            "--queries" => { queries = args[i + 1].clone(); i += 2; }
            "--tranco" => { tranco = args[i + 1].clone(); i += 2; }
            "--now" => { now_opt = Some(args[i + 1].parse().unwrap()); i += 2; }
            "--bench" => { bench = true; i += 1; }
            "--scores" => { scores = true; i += 1; }
            _ => { i += 1; }
        }
    }
    let now = now_opt.unwrap_or_else(|| Utc::now().timestamp() as f64);

    let domain_rank = load_tranco(&tranco);
    let c = load_corpus(&corpus);
    let queries_list = load_queries(&queries);

    if bench {
        let repeats = 500usize;
        let mut timings = Vec::new();
        for (_, text) in &queries_list {
            let t0 = Instant::now();
            for _ in 0..repeats {
                rank(text, &c, &domain_rank, now);
            }
            timings.push(t0.elapsed().as_secs_f64() / repeats as f64);
        }
        timings.sort_by(|a, b| a.total_cmp(b));
        let p50 = timings[timings.len() / 2];
        let idx95 = ((timings.len() as f64 * 0.95) as usize).min(timings.len() - 1);
        let p95 = timings[idx95];
        println!("bench repeats_per_query={} queries={}", repeats, queries_list.len());
        println!("latency_ms p50={:.3} p95={:.3}", p50 * 1000.0, p95 * 1000.0);
        return;
    }

    let mut out = serde_json::Map::new();
    for (qid, text) in &queries_list {
        let ranked = rank(text, &c, &domain_rank, now);
        if scores {
            let arr: Vec<Value> = ranked
                .iter()
                .map(|&(i, sc)| {
                    let mut m = serde_json::Map::new();
                    m.insert("id".into(), Value::String(c.docs[i].id.clone()));
                    m.insert("score".into(), Value::from(sc));
                    Value::Object(m)
                })
                .collect();
            out.insert(qid.clone(), Value::Array(arr));
        } else {
            let ids: Vec<String> = ranked.iter().map(|&(i, _)| c.docs[i].id.clone()).collect();
            out.insert(qid.clone(), Value::Array(ids.into_iter().map(Value::String).collect()));
        }
    }
    let val = Value::Object(out);
    println!("{}", serde_json::to_string(&val).unwrap());
}
