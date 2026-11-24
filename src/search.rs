use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use tokio::task::JoinSet;
use url::Url;

use crate::discover::{
    Candidate, CircuitBreaker, DiscoveryResult, XmlEntry, base_domain, discover, health_query_for,
    run_site, to_rank_inputs,
};
use crate::extract::{SOFT_MIN_CHARS, classify_fetch, extract_text};
use crate::http::{Fetcher, GetOpts, now_epoch};
use crate::index::SiteIndex;
use crate::planner::{Registry, load_registry, load_tranco};
use crate::ranker::rank_docs;
use crate::scout::SourceTrustStore;
use crate::text::{ranker_query, sha16};

pub const QUERY_TTL_S: f64 = 6.0 * 3600.0;
pub const CONTENT_TTL_S: f64 = 7.0 * 24.0 * 3600.0;

pub struct PipelineCache {
    conn: Mutex<Connection>,
    pub query_ttl: f64,
    pub content_ttl: f64,
}

impl PipelineCache {
    pub fn default_path() -> PathBuf {
        PathBuf::from("data").join("pipeline_cache.sqlite")
    }

    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
        let conn = Connection::open(path).map_err(|e| format!("open pipeline cache: {e}"))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=30000;
             CREATE TABLE IF NOT EXISTS cache(
                 url_hash TEXT, query_hash TEXT, url TEXT, domain TEXT,
                 markdown TEXT, status TEXT, fetched_at REAL,
                 PRIMARY KEY(url_hash, query_hash));
             CREATE TABLE IF NOT EXISTS qcache(
                 query_hash TEXT PRIMARY KEY, payload TEXT, cached_at REAL);",
        )
        .map_err(|e| format!("init pipeline cache: {e}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
            query_ttl: QUERY_TTL_S,
            content_ttl: CONTENT_TTL_S,
        })
    }

    pub fn get_query(&self, query_hash: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(String, f64)> = conn
            .query_row(
                "SELECT payload, cached_at FROM qcache WHERE query_hash=?1",
                params![query_hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();
        let (payload, cached_at) = row?;
        if self.query_ttl >= 0.0 && now_epoch() - cached_at > self.query_ttl {
            return None;
        }
        Some(payload)
    }

    pub fn set_query(&self, query_hash: &str, payload: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO qcache(query_hash, payload, cached_at) VALUES (?1,?2,?3)",
            params![query_hash, payload, now_epoch()],
        );
    }

    pub fn get_content(&self, url_hash: &str, query_hash: &str) -> Option<(String, String)> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(String, String, f64)> = conn
            .query_row(
                "SELECT markdown, status, fetched_at FROM cache WHERE url_hash=?1 AND query_hash=?2",
                params![url_hash, query_hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .ok()
            .flatten();
        let (md, status, fetched_at) = row?;
        if self.content_ttl >= 0.0 && now_epoch() - fetched_at > self.content_ttl {
            return None;
        }
        Some((md, status))
    }

    pub fn set_content(
        &self,
        url_hash: &str,
        query_hash: &str,
        url: &str,
        domain: &str,
        markdown: &str,
        status: &str,
    ) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO cache(url_hash,query_hash,url,domain,markdown,status,fetched_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![url_hash, query_hash, url, domain, markdown, status, now_epoch()],
        );
    }

    pub fn clear_queries(&self) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute("DELETE FROM qcache", []);
    }
}

pub struct SearchConfig {
    pub registry_path: PathBuf,
    pub tranco_path: PathBuf,
    pub index_path: Option<PathBuf>,
    pub cache_path: PathBuf,
    pub fetch_cache_path: PathBuf,
    pub sources_path: PathBuf,
    pub page_budget_ms: u64,
    pub max_sites: usize,
    pub per_domain_limit: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            registry_path: PathBuf::from("registry.yaml"),
            tranco_path: PathBuf::from("data").join("tranco_top100k.txt"),
            index_path: Some(SiteIndex::default_path()),
            cache_path: PipelineCache::default_path(),
            fetch_cache_path: Fetcher::default_cache_path(),
            sources_path: SourceTrustStore::default_path(),
            page_budget_ms: 0,
            max_sites: 6,
            per_domain_limit: 2,
        }
    }
}

pub struct Searcher {
    pub registry: Registry,
    pub tranco: HashMap<String, u32>,
    pub site_languages: HashMap<String, String>,
    pub index: Option<Arc<SiteIndex>>,
    pub fetcher: Arc<Fetcher>,
    pub cache: PipelineCache,
    pub trust: Option<Arc<SourceTrustStore>>,
    pub breaker: Arc<CircuitBreaker>,
    pub sitemap_mem: Arc<Mutex<HashMap<String, (Instant, String, Vec<XmlEntry>)>>>,
    pub page_budget_ms: u64,
    pub max_sites: usize,
    pub per_domain_limit: usize,
}

impl Searcher {
    pub fn open(cfg: SearchConfig) -> Result<Self, String> {
        let registry = load_registry(&cfg.registry_path)?;
        let site_languages = registry
            .sites
            .iter()
            .map(|s| (s.domain.to_ascii_lowercase(), s.language.clone()))
            .collect();
        let tranco = load_tranco(&cfg.tranco_path);
        let index = match cfg.index_path {
            Some(ref p) => Some(Arc::new(SiteIndex::open(p)?)),
            None => None,
        };
        let fetcher = Arc::new(Fetcher::open(&cfg.fetch_cache_path)?);
        let cache = PipelineCache::open(&cfg.cache_path)?;
        let trust = SourceTrustStore::open(&cfg.sources_path).ok().map(Arc::new);
        Ok(Self {
            registry,
            tranco,
            site_languages,
            index,
            fetcher,
            cache,
            trust,
            breaker: Arc::new(CircuitBreaker::new(3, 300.0)),
            sitemap_mem: Arc::new(Mutex::new(HashMap::new())),
            page_budget_ms: cfg.page_budget_ms,
            max_sites: cfg.max_sites,
            per_domain_limit: cfg.per_domain_limit,
        })
    }

    fn diversify(
        &self,
        ranked_ids: &[(String, f64)],
        orig: &HashMap<String, &Candidate>,
        k: usize,
    ) -> Vec<(String, f64)> {
        let mut chosen = Vec::new();
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut last_domain = String::new();
        let mut run = 0usize;

        for (id, score) in ranked_ids {
            if *score <= 0.0 {
                continue;
            }
            let Some(cand) = orig.get(id) else {
                continue;
            };
            let dom = base_domain(&cand.url);
            if counts.get(&dom).copied().unwrap_or(0) >= self.per_domain_limit {
                continue;
            }
            if dom == last_domain && run >= 2 {
                continue;
            }
            chosen.push((id.clone(), *score));
            *counts.entry(dom.clone()).or_insert(0) += 1;
            if dom == last_domain {
                run += 1;
            } else {
                last_domain = dom;
                run = 1;
            }
            if chosen.len() >= k {
                break;
            }
        }
        if chosen.len() < k {
            for (id, score) in ranked_ids {
                if chosen.len() >= k {
                    break;
                }
                if *score <= 0.0 || chosen.iter().any(|(cid, _)| cid == id) {
                    continue;
                }
                chosen.push((id.clone(), *score));
            }
        }
        chosen
    }

    pub async fn search(
        &self,
        query: &str,
        k: usize,
        fetch_pages: bool,
        budget_override_ms: Option<u64>,
    ) -> Value {
        let t0 = Instant::now();
        let qh = sha16(&format!("registry|{k}|{query}"));
        if fetch_pages
            && let Some(cached_raw) = self.cache.get_query(&qh)
            && let Ok(mut payload) = serde_json::from_str::<Value>(&cached_raw)
        {
            let ms = (t0.elapsed().as_secs_f64() * 1000.0 * 1000.0).round() / 1000.0;
            if let Some(arr) = payload["results"].as_array_mut() {
                for r in arr {
                    r["cache_hit"] = json!(true);
                    r["latency_ms"] = json!(ms);
                }
            }
            payload["elapsed_ms"] = json!(ms);
            return payload;
        }

        let f_req0 = self
            .fetcher
            .stats
            .requests
            .load(std::sync::atomic::Ordering::Relaxed);
        let f_bytes0 = self
            .fetcher
            .stats
            .bytes
            .load(std::sync::atomic::Ordering::Relaxed);
        let f_rob0 = self
            .fetcher
            .stats
            .robots_skips
            .load(std::sync::atomic::Ordering::Relaxed);

        let trust_map = self
            .trust
            .as_ref()
            .map(|t| t.trust_map())
            .unwrap_or_default();
        let disc: DiscoveryResult = discover(
            query,
            &self.registry,
            &self.tranco,
            &self.fetcher,
            self.index.as_ref(),
            &self.breaker,
            &self.sitemap_mem,
            &trust_map,
            self.max_sites,
            160,
        )
        .await;

        let disc_fetches = self
            .fetcher
            .stats
            .requests
            .load(std::sync::atomic::Ordering::Relaxed)
            - f_req0;
        let disc_bytes = self
            .fetcher
            .stats
            .bytes
            .load(std::sync::atomic::Ordering::Relaxed)
            - f_bytes0;
        let disc_rob = self
            .fetcher
            .stats
            .robots_skips
            .load(std::sync::atomic::Ordering::Relaxed)
            - f_rob0;

        let rank_inputs = to_rank_inputs(&disc.candidates);
        let r_query = ranker_query(query);
        let ranked = rank_docs(&r_query, &rank_inputs, &self.tranco, now_epoch());
        let orig: HashMap<String, &Candidate> = disc
            .candidates
            .iter()
            .enumerate()
            .map(|(i, c)| (format!("c{i:04}"), c))
            .collect();
        let pairs: Vec<(String, f64)> = ranked.into_iter().map(|r| (r.id, r.score)).collect();
        let chosen = self.diversify(&pairs, &orig, k);

        let mut results: Vec<Value> = Vec::new();
        let mut to_fetch: Vec<(String, &Candidate, f64)> = Vec::new();
        let mut p_fetches = 0u64;
        let mut p_bytes = 0u64;
        let mut p_rob = 0u64;

        if !fetch_pages {
            for (cid, score) in chosen {
                if let Some(cand) = orig.get(&cid) {
                    results.push(make_result(cand, score, "", json!("not-fetched"), t0, false, &cid));
                }
            }
        } else {
            for (cid, score) in &chosen {
                let Some(&cand) = orig.get(cid) else { continue };
                let uh = sha16(&cand.url);
                if !cand.body.is_empty() {
                    let dom = url_host(&cand.url);
                    self.cache
                        .set_content(&uh, &qh, &cand.url, &dom, &cand.body, "200");
                    results.push(make_result(cand, *score, &cand.body, json!(200), t0, false, cid));
                    continue;
                }
                if let Some((md, st)) = self.cache.get_content(&uh, &qh) {
                    let st_val = st
                        .parse::<u16>()
                        .map(|n| json!(n))
                        .unwrap_or_else(|_| json!(st));
                    results.push(make_result(cand, *score, &md, st_val, t0, true, cid));
                } else {
                    to_fetch.push((cid.clone(), cand, *score));
                }
            }

            if !to_fetch.is_empty() {
                let budget_ms = budget_override_ms.unwrap_or(self.page_budget_ms);
                let remaining_ms = if budget_ms > 0 {
                    budget_ms
                        .saturating_sub((t0.elapsed().as_secs_f64() * 1000.0) as u64)
                        .max(400)
                } else {
                    0
                };
                let urls: Vec<String> = to_fetch.iter().map(|(_, c, _)| c.url.clone()).collect();
                let mut fetched = fetch_many(&self.fetcher, &urls, remaining_ms).await;
                if fetched.is_empty() && budget_ms > 0 && !urls.is_empty() {
                    fetched = fetch_many(&self.fetcher, &urls[..1], 0).await;
                }
                for (cid, cand, score) in to_fetch {
                    if let Some(fr) = fetched.remove(&cand.url) {
                        if !fr.from_cache {
                            p_fetches += 1;
                            p_bytes += fr.html_bytes as u64;
                            if let Some(ref ts) = self.trust {
                                ts.record_fetch_verdict(&cand.url, fr.verdict);
                            }
                        }
                        if fr.status_tag == "robots" {
                            p_rob += 1;
                        }
                        let uh = sha16(&cand.url);
                        let dom = url_host(&cand.url);
                        let st_str = fr
                            .status_num
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| fr.status_tag.clone());
                        let clean_md = if fr.verdict == "soft-block" {
                            String::new()
                        } else {
                            fr.markdown.clone()
                        };
                        if st_str != "ERR" && fr.verdict != "soft-block" {
                            self.cache
                                .set_content(&uh, &qh, &cand.url, &dom, &clean_md, &st_str);
                        }
                        let st_val = fr
                            .status_num
                            .map(|n| json!(n))
                            .unwrap_or_else(|| json!(fr.status_tag));
                        results.push(make_result(cand, score, &clean_md, st_val, t0, false, &cid));
                    }
                }
                let order: HashMap<&str, usize> = chosen
                    .iter()
                    .enumerate()
                    .map(|(i, (cid, _))| (cid.as_str(), i))
                    .collect();
                results.sort_by_key(|r| {
                    r["candidate_id"]
                        .as_str()
                        .and_then(|c| order.get(c))
                        .copied()
                        .unwrap_or(999)
                });
            }
        }

        let elapsed_ms = (t0.elapsed().as_secs_f64() * 1000.0 * 1000.0).round() / 1000.0;
        let payload = json!({
            "query": query,
            "elapsed_ms": elapsed_ms,
            "results": results,
            "plan": {
                "language": disc.plan.language,
                "topics": disc.plan.topics,
                "sites": disc.plan.sites,
                "fallback": disc.plan.fallback,
                "open_web": disc.open_web_used,
                "sources_with_results": disc.sources_with_results,
            },
            "fetch_stats": {
                "discovery_fetches": disc_fetches,
                "discovery_bytes": disc_bytes,
                "page_fetches": p_fetches,
                "page_bytes": p_bytes,
                "total_fetches": disc_fetches + p_fetches,
                "total_bytes": disc_bytes + p_bytes,
                "robots_skips": disc_rob + p_rob,
            }
        });
        let has_err = results
            .iter()
            .any(|r| r["status"].as_str() == Some("ERR"));
        if fetch_pages && !results.is_empty() && !has_err {
            self.cache.set_query(&qh, &payload.to_string());
        }
        payload
    }

    pub async fn fetch_one_page(&self, raw_url: &str) -> Value {
        let res = self.fetcher.get(raw_url, GetOpts::default()).await;
        if res.robots_skipped {
            return json!({
                "url": raw_url,
                "ok": false,
                "robots_allowed": false,
                "error": "robots.txt disallows this URL; not fetched",
            });
        }
        if !res.ok() {
            let reason = res
                .error
                .unwrap_or_else(|| format!("HTTP {}", res.status_tag));
            return json!({
                "url": raw_url,
                "ok": false,
                "robots_allowed": res.robots_allowed,
                "status": res.status,
                "error": reason,
            });
        }
        let html = res.text;
        let final_u = res.final_url.clone();
        let text = tokio::task::spawn_blocking(move || {
            extract_text(&html, Some(&final_u), SOFT_MIN_CHARS)
        })
        .await
        .unwrap_or_default();
        if text.trim().is_empty() {
            return json!({
                "url": raw_url,
                "ok": false,
                "robots_allowed": true,
                "status": res.status,
                "error": "no extractable main text",
            });
        }
        let chars = text.chars().count();
        let verdict = classify_fetch(res.status, &res.status_tag, chars, &text);
        if !res.from_cache
            && let Some(ref ts) = self.trust
        {
            ts.record_fetch_verdict(&res.final_url, verdict);
        }
        let truncated = chars > 8000;
        let clipped: String = text.chars().take(8000).collect();
        json!({
            "url": raw_url,
            "final_url": res.final_url,
            "ok": true,
            "robots_allowed": true,
            "status": res.status,
            "verdict": verdict,
            "cached": res.from_cache,
            "chars": chars,
            "truncated": truncated,
            "text": clipped,
        })
    }

    pub async fn health(&self, probe: bool) -> Value {
        let mut by_lang: HashMap<String, usize> = HashMap::new();
        for s in &self.registry.sites {
            *by_lang.entry(s.language.clone()).or_insert(0) += 1;
        }
        let idx_report = match self.index {
            Some(ref idx) => {
                let mut val = serde_json::to_value(idx.stats()).unwrap_or(json!(null));
                if let Some(obj) = val.as_object_mut() {
                    obj.insert("path".into(), json!(idx.path.display().to_string()));
                }
                val
            }
            None => json!({"missing": true, "pages": 0}),
        };
        let snap = self.breaker.snapshot();
        let mut circuit_open: Vec<String> = snap
            .iter()
            .filter_map(|(k, v)| v.open.then(|| k.clone()))
            .collect();
        circuit_open.sort();

        let trust_stats = self
            .trust
            .as_ref()
            .map(|t| t.stats())
            .unwrap_or_else(|| json!(null));

        let mut report = json!({
            "ok": true,
            "sites": self.registry.sites.len(),
            "skipped_sites": self.registry.skipped.iter().map(|s| &s.domain).collect::<Vec<_>>(),
            "by_language": by_lang,
            "tranco_domains": self.tranco.len(),
            "ranker": { "in_process": true },
            "sources_trust": trust_stats,
            "index": idx_report,
            "circuit_open": circuit_open,
            "circuit": snap,
            "fetcher": self.fetcher.stats.snapshot(),
        });

        if probe {
            let mut set = JoinSet::new();
            for site in self.registry.sites.clone() {
                let f = self.fetcher.clone();
                let sm = self.sitemap_mem.clone();
                let idx_opt = self.index.clone();
                set.spawn(async move {
                    let t0 = Instant::now();
                    let q = health_query_for(&site.id);
                    let (method, status, links) = if let Some(ref idx) = idx_opt
                        && site.methods.iter().any(|m| m.kind == "sitemap" || m.kind == "rss")
                        && idx.site_pages(&site.id) > 0
                    {
                        let sid = site.id.clone();
                        let idx_c = idx.clone();
                        let cands = tokio::task::spawn_blocking(move || {
                            idx_c.query(&[sid], q, 20, &HashMap::new())
                        })
                        .await
                        .unwrap_or_default();
                        ("index".to_string(), if cands.is_empty() { "no-results" } else { "ok" }.to_string(), cands.len())
                    } else {
                        let outcomes = run_site(&site, q, &f, &sm).await;
                        let links: usize = outcomes.iter().map(|o| o.candidates.len()).sum();
                        let m = outcomes.first().map(|o| o.method.clone()).unwrap_or_else(|| "?".into());
                        let st = outcomes.last().map(|o| o.status.clone()).unwrap_or_else(|| "no-method".into());
                        (m, st, links)
                    };
                    json!({
                        "site": site.id,
                        "method": method,
                        "status": status,
                        "links": links,
                        "secs": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
                    })
                });
            }
            let mut probes = Vec::new();
            while let Some(res) = set.join_next().await {
                if let Ok(v) = res {
                    probes.push(v);
                }
            }
            probes.sort_by(|a, b| {
                a["site"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["site"].as_str().unwrap_or(""))
            });
            let all_ok = probes.iter().all(|p| p["links"].as_u64().unwrap_or(0) > 0);
            report["ok"] = json!(all_ok);
            report["probe"] = json!(probes);
        }
        report
    }
}

struct FetchedPage {
    status_num: Option<u16>,
    status_tag: String,
    html_bytes: usize,
    markdown: String,
    verdict: &'static str,
    from_cache: bool,
}

async fn fetch_many(
    fetcher: &Arc<Fetcher>,
    urls: &[String],
    budget_ms: u64,
) -> HashMap<String, FetchedPage> {
    let mut set = JoinSet::new();
    for u in urls.iter().cloned() {
        let f = fetcher.clone();
        set.spawn(async move {
            let res = f.get(&u, GetOpts::default()).await;
            if res.robots_skipped {
                return (
                    u,
                    FetchedPage {
                        status_num: None,
                        status_tag: "robots".into(),
                        html_bytes: 0,
                        markdown: String::new(),
                        verdict: "robots-denied",
                        from_cache: false,
                    },
                );
            }
            let md = if res.ok() {
                let html = res.text;
                let url_for_ext = u.clone();
                tokio::task::spawn_blocking(move || {
                    extract_text(&html, Some(&url_for_ext), SOFT_MIN_CHARS)
                })
                    .await
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let verdict = classify_fetch(res.status, &res.status_tag, md.chars().count(), &md);
            (
                u,
                FetchedPage {
                    status_num: res.status,
                    status_tag: res.status_tag,
                    html_bytes: res.body.len(),
                    markdown: md,
                    verdict,
                    from_cache: res.from_cache,
                },
            )
        });
    }

    let mut out = HashMap::new();
    if budget_ms > 0 {
        let deadline = tokio::time::sleep(Duration::from_millis(budget_ms));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => {
                    set.abort_all();
                    break;
                }
                next = set.join_next() => {
                    match next {
                        Some(Ok((u, page))) => { out.insert(u, page); }
                        Some(Err(_)) => {}
                        None => break,
                    }
                }
            }
        }
    } else {
        while let Some(res) = set.join_next().await {
            if let Ok((u, page)) = res {
                out.insert(u, page);
            }
        }
    }
    out
}

fn make_result(
    cand: &Candidate,
    score: f64,
    markdown: &str,
    status: Value,
    t0: Instant,
    cache_hit: bool,
    candidate_id: &str,
) -> Value {
    let ms = (t0.elapsed().as_secs_f64() * 1000.0 * 1000.0).round() / 1000.0;
    json!({
        "url": cand.url,
        "domain": url_host(&cand.url),
        "title": cand.title,
        "snippet": cand.snippet,
        "published": cand.published,
        "source": cand.source,
        "method": cand.method,
        "rank_score": score,
        "clean_markdown": markdown,
        "status": status,
        "latency_ms": ms,
        "cache_hit": cache_hit,
        "candidate_id": candidate_id,
    })
}

fn url_host(raw: &str) -> String {
    Url::parse(raw)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_cache_roundtrip_and_clear() {
        let dir = std::env::temp_dir().join(format!("magnet-pipe-{}", rand::random::<u64>()));
        let cache = PipelineCache::open(&dir.join("pipe.sqlite")).unwrap();
        cache.set_query("q1", r#"{"ok":true}"#);
        assert_eq!(cache.get_query("q1").as_deref(), Some(r#"{"ok":true}"#));
        cache.set_content("u1", "q1", "https://example.com/a", "example.com", "body", "200");
        assert_eq!(
            cache.get_content("u1", "q1"),
            Some(("body".to_string(), "200".to_string()))
        );
        cache.clear_queries();
        assert!(cache.get_query("q1").is_none());
        assert!(cache.get_content("u1", "q1").is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
