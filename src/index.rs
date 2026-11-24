use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use chrono::DateTime;
use regex::Regex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use url::Url;

use crate::discover::{
    Candidate, is_asset_url, parse_xml, score_candidates, select_sitemap_children,
};
use crate::http::{Fetcher, GetOpts, now_epoch};
use crate::planner::{Registry, Site};
use crate::text::{is_model_token, iso_date, product_aliases, query_terms, ranker_text, stem_token, unslug};

const SCHEMA_VERSION: &str = "2";
const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS pages(
    rowid INTEGER PRIMARY KEY, url TEXT UNIQUE NOT NULL, site TEXT NOT NULL,
    title TEXT, snippet TEXT, lastmod TEXT, method TEXT, text TEXT NOT NULL,
    updated REAL);
CREATE TABLE IF NOT EXISTS feeds(
    url TEXT PRIMARY KEY, site TEXT, kind TEXT, etag TEXT, lastmod TEXT,
    entries INTEGER, fetched_at REAL);
CREATE TABLE IF NOT EXISTS index_meta(key TEXT PRIMARY KEY, value TEXT);
CREATE VIRTUAL TABLE IF NOT EXISTS pages_fts
    USING fts5(url UNINDEXED, site, text, tokenize='unicode61');
CREATE INDEX IF NOT EXISTS pages_site ON pages(site);
";

#[derive(Clone, Debug, Serialize)]
pub struct IndexStats {
    pub pages: usize,
    pub sites: usize,
    pub feeds: usize,
    pub age_seconds: Option<f64>,
    pub per_site: HashMap<String, usize>,
}

#[derive(Clone, Debug)]
pub struct IndexRow {
    pub url: String,
    pub title: String,
    pub snippet: String,
    pub lastmod: Option<String>,
    pub method: String,
    pub text: String,
}

pub struct SiteIndex {
    pub path: PathBuf,
    conn: Mutex<Connection>,
}

impl SiteIndex {
    pub fn default_path() -> PathBuf {
        PathBuf::from("data").join("site_index.sqlite")
    }

    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
        let conn = Connection::open(path).map_err(|e| format!("open index: {e}"))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=30000;
             PRAGMA synchronous=NORMAL;",
        )
        .map_err(|e| format!("pragma index: {e}"))?;
        conn.execute_batch(SCHEMA_SQL)
            .map_err(|e| format!("schema index: {e}"))?;
        migrate(&conn)?;
        Ok(Self {
            path: path.to_path_buf(),
            conn: Mutex::new(conn),
        })
    }

    pub fn site_pages(&self, site_id: &str) -> usize {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM pages WHERE site=?1",
            params![site_id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0) as usize
    }

    pub fn stats(&self) -> IndexStats {
        let conn = self.conn.lock().unwrap();
        let mut per_site = HashMap::new();
        let mut pages = 0usize;
        if let Ok(mut stmt) =
            conn.prepare("SELECT site, COUNT(*) FROM pages GROUP BY site ORDER BY COUNT(*) DESC")
            && let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        {
            for (site, cnt) in rows.flatten() {
                let c = cnt.max(0) as usize;
                pages += c;
                per_site.insert(site, c);
            }
        }
        let sites = per_site.len();
        let feeds: i64 = conn
            .query_row("SELECT COUNT(*) FROM feeds", [], |r| r.get(0))
            .unwrap_or(0);
        let newest: Option<f64> = conn
            .query_row(
                "SELECT CAST(value AS REAL) FROM index_meta WHERE key='updated'",
                [],
                |r| r.get(0),
            )
            .ok()
            .or_else(|| {
                conn.query_row("SELECT MAX(fetched_at) FROM feeds", [], |r| r.get(0))
                    .ok()
                    .flatten()
            });
        let age_seconds = newest.map(|t| ((now_epoch() - t) * 10.0).round() / 10.0);
        IndexStats {
            pages,
            sites,
            feeds: feeds as usize,
            age_seconds,
            per_site,
        }
    }

    pub fn replace_site(&self, site_id: &str, rows: Vec<IndexRow>, full: bool) -> Result<usize, String> {
        let mut dedup: HashMap<String, IndexRow> = HashMap::new();
        for row in rows {
            dedup.insert(row.url.clone(), row);
        }
        let rows: Vec<IndexRow> = dedup.into_values().collect();
        if full && rows.is_empty() {
            return Ok(0);
        }
        let now = now_epoch();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(|e| format!("tx: {e}"))?;
        if full {
            tx.execute(
                "DELETE FROM pages_fts WHERE rowid IN (SELECT rowid FROM pages WHERE site=?1)",
                params![site_id],
            )
            .map_err(|e| format!("del fts: {e}"))?;
            tx.execute("DELETE FROM pages WHERE site=?1", params![site_id])
                .map_err(|e| format!("del pages: {e}"))?;
            let base: i64 = tx
                .query_row("SELECT COALESCE(MAX(rowid), 0) FROM pages", [], |r| r.get(0))
                .unwrap_or(0);
            let mut ins_p = tx
                .prepare(
                    "INSERT INTO pages(rowid,url,site,title,snippet,lastmod,method,text,updated)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                )
                .map_err(|e| format!("prep pages: {e}"))?;
            let mut ins_f = tx
                .prepare("INSERT INTO pages_fts(rowid,url,site,text) VALUES (?1,?2,?3,?4)")
                .map_err(|e| format!("prep fts: {e}"))?;
            for (idx, row) in rows.iter().enumerate() {
                let rowid = base + idx as i64 + 1;
                ins_p
                    .execute(params![
                        rowid,
                        row.url,
                        site_id,
                        row.title,
                        row.snippet,
                        row.lastmod,
                        row.method,
                        row.text,
                        now
                    ])
                    .map_err(|e| format!("ins page: {e}"))?;
                ins_f
                    .execute(params![rowid, row.url, site_id, row.text])
                    .map_err(|e| format!("ins fts: {e}"))?;
            }
        } else {
            for row in &rows {
                let existing: Option<i64> = tx
                    .query_row(
                        "SELECT rowid FROM pages WHERE url=?1",
                        params![row.url],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|e| format!("sel url: {e}"))?;
                let rowid = if let Some(rid) = existing {
                    tx.execute(
                        "UPDATE pages SET site=?1,title=?2,snippet=?3,lastmod=?4,method=?5,text=?6,updated=?7 WHERE rowid=?8",
                        params![site_id, row.title, row.snippet, row.lastmod, row.method, row.text, now, rid],
                    )
                    .map_err(|e| format!("upd page: {e}"))?;
                    tx.execute("DELETE FROM pages_fts WHERE rowid=?1", params![rid])
                        .map_err(|e| format!("del fts row: {e}"))?;
                    rid
                } else {
                    tx.execute(
                        "INSERT INTO pages(url,site,title,snippet,lastmod,method,text,updated) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                        params![row.url, site_id, row.title, row.snippet, row.lastmod, row.method, row.text, now],
                    )
                    .map_err(|e| format!("ins page: {e}"))?;
                    tx.last_insert_rowid()
                };
                tx.execute(
                    "INSERT INTO pages_fts(rowid,url,site,text) VALUES (?1,?2,?3,?4)",
                    params![rowid, row.url, site_id, row.text],
                )
                .map_err(|e| format!("ins fts row: {e}"))?;
            }
        }
        if !rows.is_empty() {
            let _ = tx.execute(
                "INSERT OR REPLACE INTO index_meta(key,value) VALUES ('updated',?1)",
                params![now.to_string()],
            );
        }
        tx.commit().map_err(|e| format!("commit: {e}"))?;
        Ok(rows.len())
    }

    pub fn record_feed(
        &self,
        url: &str,
        site: &str,
        kind: &str,
        etag: Option<&str>,
        lastmod: Option<&str>,
        entries: usize,
    ) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO feeds(url,site,kind,etag,lastmod,entries,fetched_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![url, site, kind, etag, lastmod, entries as i64, now_epoch()],
        );
    }

    pub fn query_sites(
        &self,
        site_ids: &[String],
        query: &str,
        max_results: usize,
        rows_per_site: usize,
        max_age_days: &HashMap<String, u64>,
    ) -> HashMap<String, Vec<Candidate>> {
        if site_ids.is_empty() {
            return HashMap::new();
        }
        let mut terms: Vec<String> = Vec::new();
        for t in query_terms(query, false) {
            let st = stem_token(&t);
            if !st.is_empty() && !terms.contains(&st) {
                terms.push(st);
            }
        }
        let compounds: HashSet<String> = terms
            .windows(2)
            .filter_map(|w| {
                let c = format!("{}{}", w[0], w[1]);
                terms.contains(&c).then_some(c)
            })
            .collect();
        let mut retrieval: Vec<String> = terms
            .into_iter()
            .filter(|t| !compounds.contains(t))
            .collect();
        for term in retrieval.clone() {
            for alias in product_aliases(&term) {
                if !retrieval.contains(&alias) {
                    retrieval.push(alias);
                }
            }
        }
        if retrieval.is_empty() {
            return HashMap::new();
        }

        let mut pairs = Vec::new();
        if retrieval.len() == 1 {
            pairs.push(format!("\"{}\"", retrieval[0]));
        }
        'outer: for (i, first) in retrieval.iter().enumerate() {
            for second in &retrieval[i + 1..] {
                pairs.push(format!("(\"{first}\" AND \"{second}\")"));
                if pairs.len() >= 12 {
                    break 'outer;
                }
            }
        }
        let all_and = retrieval
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(" AND ");
        let model_terms: Vec<String> = retrieval
            .iter()
            .filter(|t| is_model_token(t))
            .cloned()
            .collect();
        let site_expr = site_ids
            .iter()
            .map(|s| format!("site:{s}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let limit = rows_per_site.max(rows_per_site * site_ids.len().min(8)) as i64;

        let conn = self.conn.lock().unwrap();
        let fetch_rows = |inner: &str| -> Vec<(String, String, Option<String>, Option<String>, Option<String>, Option<String>)> {
            let expr = format!("({site_expr}) AND ({inner})");
            let total: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pages_fts WHERE pages_fts MATCH ?1",
                    params![expr],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let sql = if total <= limit {
                "SELECT p.site, p.url, p.title, p.snippet, p.lastmod, p.method
                 FROM pages_fts f JOIN pages p ON p.rowid=f.rowid
                 WHERE pages_fts MATCH ?1 LIMIT ?2"
            } else {
                "SELECT p.site, p.url, p.title, p.snippet, p.lastmod, p.method
                 FROM pages_fts f JOIN pages p ON p.rowid=f.rowid
                 WHERE pages_fts MATCH ?1 ORDER BY rank LIMIT ?2"
            };
            let Ok(mut stmt) = conn.prepare(sql) else {
                return Vec::new();
            };
            let Ok(mapped) = stmt.query_map(params![expr, limit], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            }) else {
                return Vec::new();
            };
            mapped.flatten().collect()
        };

        let all_expr = format!("({site_expr}) AND ({all_and})");
        let total_all: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pages_fts WHERE pages_fts MATCH ?1",
                params![all_expr],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let mut rows = if total_all >= 5 {
            fetch_rows(&all_and)
        } else {
            let mut r = fetch_rows(&pairs.join(" OR "));
            if r.is_empty() && !model_terms.is_empty() {
                let m_expr = model_terms
                    .iter()
                    .map(|t| format!("\"{t}\""))
                    .collect::<Vec<_>>()
                    .join(" OR ");
                r = fetch_rows(&m_expr);
            }
            r
        };
        drop(conn);

        let now_ts = now_epoch();
        let mut grouped: HashMap<String, Vec<Candidate>> = HashMap::new();
        for (site_id, url, title, snippet, lastmod, method) in rows.drain(..) {
            if let Some(&days) = max_age_days.get(&site_id)
                && days > 0
                && let Some(ref lm) = lastmod
                && let Ok(dt) = DateTime::parse_from_rfc3339(lm)
            {
                let age = now_ts - dt.timestamp() as f64;
                if age > days as f64 * 86400.0 {
                    continue;
                }
            }
            grouped.entry(site_id.clone()).or_default().push(Candidate {
                url: url.clone(),
                title: title.filter(|s| !s.is_empty()).unwrap_or_else(|| unslug(&url)),
                snippet: snippet.unwrap_or_default(),
                published: iso_date(lastmod.as_deref()),
                source: site_id,
                method: method.unwrap_or_else(|| "sitemap".to_string()),
                score: 0.0,
                body: String::new(),
            });
        }

        let mut out = HashMap::new();
        for (site_id, cands) in grouped {
            let ranked = score_candidates(query, cands, max_results);
            if !ranked.is_empty() {
                out.insert(site_id, ranked);
            }
        }
        out
    }

    pub fn query(
        &self,
        site_ids: &[String],
        query: &str,
        max_results: usize,
        max_age_days: &HashMap<String, u64>,
    ) -> Vec<Candidate> {
        let by_site = self.query_sites(site_ids, query, max_results, 250, max_age_days);
        let mut merged: HashMap<String, Candidate> = HashMap::new();
        for cands in by_site.into_values() {
            for cand in cands {
                let key = canonical_url(&cand.url);
                let replace = merged
                    .get(&key)
                    .map(|prev| cand.score > prev.score)
                    .unwrap_or(true);
                if replace {
                    merged.insert(key, cand);
                }
            }
        }
        score_candidates(query, merged.into_values().collect(), max_results)
    }
}

fn migrate(conn: &Connection) -> Result<(), String> {
    let ver: Option<String> = conn
        .query_row("SELECT value FROM index_meta WHERE key='schema'", [], |r| r.get(0))
        .optional()
        .map_err(|e| format!("meta: {e}"))?;
    if ver.as_deref() == Some(SCHEMA_VERSION) {
        return Ok(());
    }
    conn.execute_batch(
        "DROP TABLE IF EXISTS pages_fts;
         DROP TABLE IF EXISTS pages;
         DROP TABLE IF EXISTS feeds;",
    )
    .map_err(|e| format!("drop old schema: {e}"))?;
    conn.execute_batch(SCHEMA_SQL)
        .map_err(|e| format!("recreate schema: {e}"))?;
    conn.execute(
        "INSERT OR REPLACE INTO index_meta(key,value) VALUES ('schema',?1)",
        params![SCHEMA_VERSION],
    )
    .map_err(|e| format!("set schema: {e}"))?;
    Ok(())
}

pub fn canonical_url(raw: &str) -> String {
    let Ok(u) = Url::parse(raw) else {
        return raw.to_string();
    };
    let mut path = u.path().to_string();
    if path.is_empty() {
        path = "/".to_string();
    } else if path != "/" && path.ends_with('/') {
        path.pop();
    }
    let q = u.query().map(|q| format!("?{q}")).unwrap_or_default();
    format!(
        "{}://{}{}{}",
        u.scheme().to_ascii_lowercase(),
        u.host_str().unwrap_or("").to_ascii_lowercase(),
        path,
        q
    )
}

pub fn rows_from_entries(site: &Site, entries: &[crate::discover::XmlEntry], kind: &str) -> Vec<IndexRow> {
    let domain = site.domain.to_ascii_lowercase();
    let tag_re = Regex::new(r"<[^>]+>").unwrap();
    let mut rows = Vec::new();
    for item in entries {
        let url = item.loc.as_deref().or(item.link.as_deref()).unwrap_or("");
        if url.is_empty() || is_asset_url(url) {
            continue;
        }
        let host = Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default();
        if !host.contains(&domain) {
            continue;
        }
        let (title, snippet, lastmod, method) = if kind == "rss" || kind == "feed" {
            let t = item
                .title
                .as_deref()
                .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| unslug(url));
            let desc = tag_re.replace_all(item.description.as_deref().unwrap_or(""), " ");
            let sn = desc.split_whitespace().collect::<Vec<_>>().join(" ");
            (
                t.chars().take(300).collect::<String>(),
                sn.chars().take(500).collect::<String>(),
                item.pub_date.as_deref(),
                "rss",
            )
        } else {
            let t = item
                .title
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| unslug(url));
            let sn = format!("{} {}", site.name, unslug(url));
            (
                t.chars().take(300).collect::<String>(),
                sn.chars().take(500).collect::<String>(),
                item.lastmod.as_deref(),
                "sitemap",
            )
        };
        let text = ranker_text(&format!("{} {} {}", unslug(url), title, snippet));
        rows.push(IndexRow {
            url: url.to_string(),
            title,
            snippet,
            lastmod: iso_date(lastmod),
            method: method.to_string(),
            text,
        });
    }
    rows
}

#[derive(Serialize)]
pub struct SiteIngestReport {
    pub site: String,
    pub pages: usize,
    pub feeds: usize,
    pub robots_skips: usize,
    pub errors: Vec<String>,
    pub seconds: f64,
}

pub async fn ingest_site(
    site: &Site,
    fetcher: &Fetcher,
    index: &SiteIndex,
    full: bool,
    recent: usize,
) -> SiteIngestReport {
    let t0 = Instant::now();
    let mut rows = Vec::new();
    let mut feeds = 0usize;
    let mut robots = 0usize;
    let mut errors = Vec::new();
    let opts = GetOpts {
        conditional: true,
        ..Default::default()
    };

    for method in &site.methods {
        if method.kind == "sitemap" {
            let mut urls = method.urls.clone();
            if urls.is_empty()
                && let Some(ref u) = method.url
            {
                urls.push(u.clone());
            }
            let limit = if full {
                site.index_max_children.unwrap_or(0)
            } else {
                recent
            };
            for url in urls {
                let res = fetcher.get(&url, opts).await;
                if res.robots_skipped {
                    robots += 1;
                    continue;
                }
                if !res.ok() {
                    errors.push(format!("{url}: {}", res.error.unwrap_or(res.status_tag)));
                    continue;
                }
                index.record_feed(
                    &url,
                    &site.id,
                    "sitemapindex",
                    res.headers.get("etag").map(String::as_str),
                    res.headers.get("last-modified").map(String::as_str),
                    0,
                );
                if res.not_modified {
                    continue;
                }
                let (root_kind, entries) = parse_xml(&res.text);
                let children: Vec<String> = if root_kind == "sitemapindex" {
                    select_sitemap_children(&entries, method, limit)
                        .into_iter()
                        .filter_map(|e| e.loc)
                        .collect()
                } else {
                    vec![url.clone()]
                };
                for child_url in children {
                    let (ckind, centries, hdrs) = if child_url != url {
                        let res2 = fetcher.get(&child_url, opts).await;
                        if res2.robots_skipped {
                            robots += 1;
                            continue;
                        }
                        if !res2.ok() {
                            errors.push(format!(
                                "{child_url}: {}",
                                res2.error.unwrap_or(res2.status_tag)
                            ));
                            continue;
                        }
                        if res2.not_modified {
                            continue;
                        }
                        let (k, e) = parse_xml(&res2.text);
                        (k, e, res2.headers)
                    } else {
                        (root_kind.clone(), entries.clone(), res.headers.clone())
                    };
                    index.record_feed(
                        &child_url,
                        &site.id,
                        &ckind,
                        hdrs.get("etag").map(String::as_str),
                        hdrs.get("last-modified").map(String::as_str),
                        centries.len(),
                    );
                    feeds += 1;
                    rows.extend(rows_from_entries(site, &centries, &ckind));
                }
            }
        } else if method.kind == "rss"
            && let Some(ref url) = method.url
        {
            let res = fetcher.get(url, opts).await;
            if res.robots_skipped {
                robots += 1;
                continue;
            }
            if !res.ok() {
                errors.push(format!("{url}: {}", res.error.unwrap_or(res.status_tag)));
                continue;
            }
            index.record_feed(
                url,
                &site.id,
                "rss",
                res.headers.get("etag").map(String::as_str),
                res.headers.get("last-modified").map(String::as_str),
                0,
            );
            if !res.not_modified {
                let (fkind, entries) = parse_xml(&res.text);
                feeds += 1;
                rows.extend(rows_from_entries(site, &entries, &fkind));
            }
        }
    }

    let written = index.replace_site(&site.id, rows, full).unwrap_or(0);
    errors.truncate(8);
    SiteIngestReport {
        site: site.id.clone(),
        pages: written,
        feeds,
        robots_skips: robots,
        errors,
        seconds: (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
    }
}

pub async fn build_index(
    reg: &Registry,
    sites_filter: &[String],
    db_path: &Path,
    full: bool,
    recent: usize,
    concurrency: usize,
) -> Result<(Vec<SiteIngestReport>, IndexStats), String> {
    let index = SiteIndex::open(db_path)?;
    let fetcher = Fetcher::with_transport(
        &Fetcher::default_cache_path(),
        Box::new(crate::http::ReqwestTransport::new()?),
        concurrency,
        1.0,
    )?;
    let selected: Vec<Site> = reg
        .sites
        .iter()
        .filter(|s| sites_filter.is_empty() || sites_filter.contains(&s.id))
        .filter(|s| s.methods.iter().any(|m| m.kind == "sitemap" || m.kind == "rss"))
        .cloned()
        .collect();

    let mut set = tokio::task::JoinSet::new();
    let idx = std::sync::Arc::new(index);
    let fch = std::sync::Arc::new(fetcher);
    for site in selected {
        let idx_c = idx.clone();
        let fch_c = fch.clone();
        set.spawn(async move { ingest_site(&site, &fch_c, &idx_c, full, recent).await });
    }
    let mut reports = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(r) = res {
            reports.push(r);
        }
    }
    reports.sort_by(|a, b| a.site.cmp(&b.site));
    let stats = idx.stats();
    Ok((reports, stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::XmlEntry;
    use rand::Rng;

    #[test]
    fn fts5_index_turkish_query_and_replace() {
        let dir = std::env::temp_dir().join(format!("magnet-idx-{}", rand::rng().random::<u64>()));
        let idx = SiteIndex::open(&dir.join("index.sqlite")).unwrap();
        let site = Site {
            id: "fixture".into(),
            name: "Fixture".into(),
            domain: "example.com".into(),
            topics: vec!["boilers".into()],
            language: "tr".into(),
            priority: None,
            timeout_s: None,
            index_max_age_days: None,
            index_max_children: None,
            methods: vec![],
        };
        let entries = vec![
            XmlEntry {
                loc: Some("https://example.com/vaillant-kombi-ariza-kodu-f28".into()),
                ..Default::default()
            },
            XmlEntry {
                loc: Some("https://example.com/logo.png".into()),
                ..Default::default()
            },
        ];
        let rows = rows_from_entries(&site, &entries, "urlset");
        assert_eq!(rows.len(), 1);
        assert_eq!(idx.replace_site("fixture", rows, true).unwrap(), 1);
        let hits = idx.query(&["fixture".into()], "vaillant kombi arıza kodu", 10, &HashMap::new());
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://example.com/vaillant-kombi-ariza-kodu-f28");
        assert_eq!(idx.replace_site("fixture", vec![], true).unwrap(), 0);
        assert_eq!(idx.site_pages("fixture"), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
