use std::collections::HashMap;
use std::future::Future;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flate2::read::GzDecoder;
use rand::Rng;
use reqwest::Client;
use robotstxt::DefaultMatcher;
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use url::Url;

pub const USER_AGENT: &str = "KrikoMagnet/0.2 (+https://kriko.cc/bot)";
pub const ROBOTS_AGENT: &str = "KrikoMagnet";
pub const ROBOTS_TTL: f64 = 24.0 * 3600.0;
pub const PAGE_TTL: f64 = 24.0 * 3600.0;
pub const MAX_CACHE_BYTES: usize = 8_000_000;
pub const MAX_RETRIES: usize = 3;
pub const MAX_BACKOFF_S: f64 = 30.0;

pub fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn host_of(raw_url: &str) -> String {
    Url::parse(raw_url)
        .ok()
        .and_then(|u| {
            u.host_str().map(|h| match u.port() {
                Some(p) => format!("{}:{}", h.to_ascii_lowercase(), p),
                None => h.to_ascii_lowercase(),
            })
        })
        .unwrap_or_default()
}

pub fn origin_of(raw_url: &str) -> String {
    Url::parse(raw_url)
        .ok()
        .map(|u| {
            let host = u.host_str().unwrap_or("").to_ascii_lowercase();
            match u.port() {
                Some(p) => format!("{}://{}:{}", u.scheme(), host, p),
                None => format!("{}://{}", u.scheme(), host),
            }
        })
        .unwrap_or_default()
}

pub fn decode_body(body: &[u8], headers: &HashMap<String, String>) -> String {
    let mut bytes = body;
    let decompressed;
    if body.len() >= 2 && body[0] == 0x1f && body[1] == 0x8b {
        let mut gz = GzDecoder::new(body);
        let mut out = Vec::new();
        if gz.read_to_end(&mut out).is_ok() && !out.is_empty() {
            decompressed = out;
            bytes = &decompressed;
        }
    }
    let _ = headers;
    String::from_utf8_lossy(bytes).into_owned()
}

pub fn retry_after_seconds(value: Option<&str>, now_secs: f64) -> Option<f64> {
    let v = value?.trim();
    if v.is_empty() {
        return None;
    }
    if let Ok(secs) = v.parse::<f64>() {
        return Some(secs.max(0.0));
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(v) {
        return Some((dt.timestamp() as f64 - now_secs).max(0.0));
    }
    None
}

#[derive(Clone, Debug)]
pub struct RobotsPolicy {
    pub body: String,
    pub deny_all: bool,
    pub unknown: bool,
    pub crawl_delay_s: Option<f64>,
}

impl RobotsPolicy {
    pub fn parse(body: &str, status: i64) -> Self {
        let deny_all = matches!(status, 401 | 403 | 429) || (500..=599).contains(&status);
        let head: String = body.chars().take(300).collect();
        let is_html = head.to_ascii_lowercase().contains("<html");
        let unknown = body.trim().is_empty() || is_html;
        let crawl_delay_s = if !unknown {
            parse_crawl_delay(body, ROBOTS_AGENT)
        } else {
            None
        };
        Self {
            body: body.to_string(),
            deny_all,
            unknown,
            crawl_delay_s,
        }
    }

    pub fn allowed(&self, url: &str) -> bool {
        if self.deny_all {
            return false;
        }
        if self.unknown {
            return true;
        }
        let mut matcher = DefaultMatcher::default();
        matcher.one_agent_allowed_by_robots(&self.body, ROBOTS_AGENT, url)
    }
}

fn parse_crawl_delay(body: &str, agent: &str) -> Option<f64> {
    let agent_low = agent.to_ascii_lowercase();
    let mut in_group = false;
    let mut specific_match = false;
    let mut generic_delay: Option<f64> = None;
    let mut specific_delay: Option<f64> = None;

    for raw in body.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            in_group = false;
            specific_match = false;
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let key = k.trim().to_ascii_lowercase();
        let val = v.trim();
        if key == "user-agent" {
            let ua = val.to_ascii_lowercase();
            if ua == "*" {
                in_group = true;
            } else if agent_low.contains(&ua) || ua.contains(&agent_low) {
                in_group = true;
                specific_match = true;
            } else if !in_group {
                specific_match = false;
            }
        } else if key == "crawl-delay"
            && in_group
            && let Ok(d) = val.parse::<f64>()
            && d > 0.0
        {
            if specific_match {
                specific_delay = Some(d);
            } else if generic_delay.is_none() {
                generic_delay = Some(d);
            }
        }
    }
    specific_delay.or(generic_delay)
}

#[derive(Default)]
pub struct FetchStats {
    pub requests: AtomicU64,
    pub cache_hits: AtomicU64,
    pub robots_skips: AtomicU64,
    pub retries: AtomicU64,
    pub bytes: AtomicU64,
}

impl FetchStats {
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "requests": self.requests.load(Ordering::Relaxed),
            "cache_hits": self.cache_hits.load(Ordering::Relaxed),
            "robots_skips": self.robots_skips.load(Ordering::Relaxed),
            "retries": self.retries.load(Ordering::Relaxed),
            "bytes": self.bytes.load(Ordering::Relaxed),
        })
    }
}

#[derive(Clone, Debug)]
pub struct FetchResult {
    pub final_url: String,
    pub status: Option<u16>,
    pub status_tag: String,
    pub body: Vec<u8>,
    pub text: String,
    pub headers: HashMap<String, String>,
    pub error: Option<String>,
    pub from_cache: bool,
    pub not_modified: bool,
    pub robots_allowed: Option<bool>,
    pub robots_skipped: bool,
    pub elapsed_ms: f64,
}

impl FetchResult {
    pub fn ok(&self) -> bool {
        self.status == Some(200) && self.error.is_none()
    }
}

pub struct RawResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub headers: HashMap<String, String>,
    pub final_url: String,
}

pub trait Transport: Send + Sync {
    fn get<'a>(
        &'a self,
        url: &'a str,
        headers: &'a HashMap<String, String>,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<RawResponse, String>> + Send + 'a>>;
}

pub struct ReqwestTransport {
    client: Client,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self, String> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        Ok(Self { client })
    }
}

impl Transport for ReqwestTransport {
    fn get<'a>(
        &'a self,
        url: &'a str,
        headers: &'a HashMap<String, String>,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<RawResponse, String>> + Send + 'a>> {
        Box::pin(async move {
            let mut req = self.client.get(url).timeout(timeout);
            for (k, v) in headers {
                req = req.header(k.as_str(), v.as_str());
            }
            let resp = req.send().await.map_err(|e| e.to_string())?;
            let status = resp.status().as_u16();
            let final_url = resp.url().to_string();
            let mut hdrs = HashMap::new();
            for (k, v) in resp.headers() {
                if let Ok(vs) = v.to_str() {
                    hdrs.insert(k.as_str().to_ascii_lowercase(), vs.to_string());
                }
            }
            let bytes = resp.bytes().await.map_err(|e| e.to_string())?.to_vec();
            Ok(RawResponse {
                status,
                body: bytes,
                headers: hdrs,
                final_url,
            })
        })
    }
}

struct RateLimiter {
    min_interval: Duration,
    hosts: Mutex<HashMap<String, Arc<AsyncMutex<Option<Instant>>>>>,
}

impl RateLimiter {
    fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            hosts: Mutex::new(HashMap::new()),
        }
    }

    async fn wait(&self, host: &str, crawl_delay_s: Option<f64>) {
        let interval = crawl_delay_s
            .map(|d| self.min_interval.max(Duration::from_secs_f64(d)))
            .unwrap_or(self.min_interval);
        if interval.is_zero() {
            return;
        }
        let slot = {
            let mut map = self.hosts.lock().unwrap();
            map.entry(host.to_string())
                .or_insert_with(|| Arc::new(AsyncMutex::new(None)))
                .clone()
        };
        let mut last = slot.lock().await;
        if let Some(prev) = *last {
            let elapsed = prev.elapsed();
            if elapsed < interval {
                tokio::time::sleep(interval - elapsed).await;
            }
        }
        *last = Some(Instant::now());
    }
}

#[derive(Clone, Copy)]
pub struct GetOpts {
    pub check_robots: bool,
    pub use_cache: bool,
    pub timeout_s: f64,
    pub conditional: bool,
}

impl Default for GetOpts {
    fn default() -> Self {
        Self {
            check_robots: true,
            use_cache: true,
            timeout_s: 25.0,
            conditional: false,
        }
    }
}

pub struct Fetcher {
    conn: Mutex<Connection>,
    transport: Box<dyn Transport>,
    limiter: RateLimiter,
    sem: Semaphore,
    robots_mem: Mutex<HashMap<String, RobotsPolicy>>,
    robots_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    pub cache_ttl: f64,
    pub robots_ttl: f64,
    pub min_interval_s: f64,
    pub stats: FetchStats,
}

impl Fetcher {
    pub fn open(cache_path: &Path) -> Result<Self, String> {
        let transport = ReqwestTransport::new()?;
        Self::with_transport(cache_path, Box::new(transport), 8, 1.0)
    }

    pub fn with_transport(
        cache_path: &Path,
        transport: Box<dyn Transport>,
        concurrency: usize,
        min_interval_s: f64,
    ) -> Result<Self, String> {
        if let Some(parent) = cache_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
        let conn = Connection::open(cache_path).map_err(|e| format!("open fetch cache: {e}"))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=30000;
             CREATE TABLE IF NOT EXISTS http_cache(
                 url TEXT PRIMARY KEY, status INTEGER, headers TEXT, body BLOB, fetched_at REAL);
             CREATE TABLE IF NOT EXISTS robots(
                 origin TEXT PRIMARY KEY, status INTEGER, body TEXT, fetched_at REAL);",
        )
        .map_err(|e| format!("init fetch cache: {e}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
            transport,
            limiter: RateLimiter::new(Duration::from_secs_f64(min_interval_s.max(0.0))),
            sem: Semaphore::new(concurrency.max(1)),
            robots_mem: Mutex::new(HashMap::new()),
            robots_locks: Mutex::new(HashMap::new()),
            cache_ttl: PAGE_TTL,
            robots_ttl: ROBOTS_TTL,
            min_interval_s,
            stats: FetchStats::default(),
        })
    }

    pub fn default_cache_path() -> PathBuf {
        PathBuf::from("data").join("fetch_cache.sqlite")
    }

    fn cache_get(
        &self,
        url: &str,
        ttl: Option<f64>,
    ) -> Option<(i64, HashMap<String, String>, Vec<u8>)> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(i64, String, Vec<u8>, f64)> = conn
            .query_row(
                "SELECT status, headers, body, fetched_at FROM http_cache WHERE url=?1",
                params![url],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .ok()
            .flatten();
        let (status, hdrs_raw, body, fetched_at) = row?;
        if let Some(max_age) = ttl
            && (max_age <= 0.0 || now_epoch() - fetched_at > max_age)
        {
            return None;
        }
        let hdrs: HashMap<String, String> = serde_json::from_str(&hdrs_raw).unwrap_or_default();
        Some((status, hdrs, body))
    }

    fn cache_set(&self, url: &str, status: i64, headers: &HashMap<String, String>, body: &[u8]) {
        if body.len() > MAX_CACHE_BYTES {
            return;
        }
        let hdrs = serde_json::to_string(headers).unwrap_or_else(|_| "{}".to_string());
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO http_cache(url,status,headers,body,fetched_at) VALUES (?1,?2,?3,?4,?5)",
            params![url, status, hdrs, body, now_epoch()],
        );
    }

    fn cache_touch(&self, url: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE http_cache SET fetched_at=?1 WHERE url=?2",
            params![now_epoch(), url],
        );
    }

    fn robots_db_get(&self, origin: &str) -> Option<(i64, String)> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(i64, String, f64)> = conn
            .query_row(
                "SELECT status, body, fetched_at FROM robots WHERE origin=?1",
                params![origin],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .ok()
            .flatten();
        let (status, body, fetched_at) = row?;
        if now_epoch() - fetched_at > self.robots_ttl {
            return None;
        }
        Some((status, body))
    }

    fn robots_db_set(&self, origin: &str, status: i64, body: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO robots(origin,status,body,fetched_at) VALUES (?1,?2,?3,?4)",
            params![origin, status, body, now_epoch()],
        );
    }

    pub async fn robots_policy(&self, url: &str) -> RobotsPolicy {
        let origin = origin_of(url);
        if let Some(p) = self.robots_mem.lock().unwrap().get(&origin).cloned() {
            return p;
        }
        let lock = {
            let mut map = self.robots_locks.lock().unwrap();
            map.entry(origin.clone())
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;
        if let Some(p) = self.robots_mem.lock().unwrap().get(&origin).cloned() {
            return p;
        }
        if let Some((status, body)) = self.robots_db_get(&origin) {
            let p = RobotsPolicy::parse(&body, status);
            self.robots_mem.lock().unwrap().insert(origin, p.clone());
            return p;
        }
        let robots_url = format!("{origin}/robots.txt");
        let host = host_of(&robots_url);
        self.limiter.wait(&host, None).await;
        let mut hdrs = HashMap::new();
        hdrs.insert("User-Agent".into(), USER_AGENT.into());
        let resp = {
            let _perm = self.sem.acquire().await.unwrap();
            self.transport
                .get(&robots_url, &hdrs, Duration::from_secs(20))
                .await
        };
        let (status, body) = match resp {
            Ok(r) => {
                self.stats.requests.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .bytes
                    .fetch_add(r.body.len() as u64, Ordering::Relaxed);
                let txt = decode_body(&r.body, &r.headers);
                (r.status as i64, txt)
            }
            Err(_) => (0, String::new()),
        };
        self.robots_db_set(&origin, status, &body);
        let policy = RobotsPolicy::parse(&body, status);
        self.robots_mem
            .lock()
            .unwrap()
            .insert(origin, policy.clone());
        policy
    }

    pub async fn get(&self, url: &str, opts: GetOpts) -> FetchResult {
        let t0 = Instant::now();
        let mut out = FetchResult {
            final_url: url.to_string(),
            status: None,
            status_tag: "ERR".to_string(),
            body: Vec::new(),
            text: String::new(),
            headers: HashMap::new(),
            error: None,
            from_cache: false,
            not_modified: false,
            robots_allowed: None,
            robots_skipped: false,
            elapsed_ms: 0.0,
        };
        let mut crawl_delay = None;
        if opts.check_robots {
            let policy = self.robots_policy(url).await;
            let allowed = policy.allowed(url);
            out.robots_allowed = Some(allowed);
            crawl_delay = policy.crawl_delay_s;
            if !allowed {
                self.stats.robots_skips.fetch_add(1, Ordering::Relaxed);
                out.robots_skipped = true;
                out.status_tag = "robots-denied".to_string();
                out.elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
                return out;
            }
        }

        let mut stale: Option<(i64, HashMap<String, String>, Vec<u8>)> = None;
        if opts.use_cache {
            if let Some((status, hdrs, body)) = self.cache_get(url, Some(self.cache_ttl)) {
                self.stats.cache_hits.fetch_add(1, Ordering::Relaxed);
                out.status = Some(status as u16);
                out.status_tag = status.to_string();
                out.text = decode_body(&body, &hdrs);
                out.body = body;
                out.headers = hdrs;
                out.from_cache = true;
                out.elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
                return out;
            }
            if opts.conditional {
                stale = self.cache_get(url, None);
            }
        }

        let mut req_headers = HashMap::new();
        req_headers.insert("User-Agent".into(), USER_AGENT.into());
        if let Some((_, ref old_hdrs, _)) = stale {
            let etag = old_hdrs.get("etag");
            let last_mod = old_hdrs.get("last-modified");
            if let Some(e) = etag {
                req_headers.insert("If-None-Match".into(), e.clone());
            }
            if let Some(lm) = last_mod {
                req_headers.insert("If-Modified-Since".into(), lm.clone());
            }
            if etag.is_none() && last_mod.is_none() {
                stale = None;
            }
        }

        let host = host_of(url);
        let timeout = Duration::from_secs_f64(opts.timeout_s.max(1.0));
        for attempt in 0..=MAX_RETRIES {
            self.limiter.wait(&host, crawl_delay).await;
            let resp = {
                let _perm = self.sem.acquire().await.unwrap();
                self.transport.get(url, &req_headers, timeout).await
            };
            let raw = match resp {
                Ok(r) => r,
                Err(err) => {
                    out.error = Some(err.chars().take(200).collect());
                    out.status = None;
                    out.status_tag = "ERR".to_string();
                    if attempt < MAX_RETRIES {
                        self.stats.retries.fetch_add(1, Ordering::Relaxed);
                        let jitter: f64 = rand::rng().random_range(0.75..=1.0);
                        let wait = (1.0 + attempt as f64) * jitter;
                        tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                        continue;
                    }
                    out.elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
                    return out;
                }
            };

            if raw.status == 304
                && let Some((_, old_hdrs, old_body)) = stale
            {
                self.stats.requests.fetch_add(1, Ordering::Relaxed);
                self.cache_touch(url);
                out.status = Some(200);
                out.status_tag = "200".to_string();
                out.text = decode_body(&old_body, &old_hdrs);
                out.body = old_body;
                out.headers = old_hdrs;
                out.from_cache = true;
                out.not_modified = true;
                out.final_url = raw.final_url;
                out.elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
                return out;
            }

            self.stats.requests.fetch_add(1, Ordering::Relaxed);
            self.stats
                .bytes
                .fetch_add(raw.body.len() as u64, Ordering::Relaxed);
            out.error = None;
            out.status = Some(raw.status);
            out.status_tag = raw.status.to_string();
            out.text = decode_body(&raw.body, &raw.headers);
            out.body = raw.body;
            out.headers = raw.headers;
            out.final_url = raw.final_url;

            if matches!(raw.status, 429 | 500 | 502 | 503 | 504) && attempt < MAX_RETRIES {
                let ra = retry_after_seconds(
                    out.headers.get("retry-after").map(String::as_str),
                    now_epoch(),
                );
                let wait = if let (Some(secs), true) = (ra, matches!(raw.status, 429 | 503)) {
                    secs.clamp(1.0, MAX_BACKOFF_S)
                } else {
                    let jitter: f64 = rand::rng().random_range(0.75..=1.0);
                    (2.0 * (attempt + 1) as f64 * jitter).min(MAX_BACKOFF_S)
                };
                self.stats.retries.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_secs_f64(wait)).await;
                continue;
            }
            break;
        }

        if out.status == Some(200) && opts.use_cache {
            self.cache_set(url, 200, &out.headers, &out.body);
        }
        out.elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct MockTransport {
        responses: Mutex<HashMap<String, Vec<RawResponse>>>,
        calls: AtomicUsize,
    }

    type MockEntry<'a> = (&'a str, u16, &'a str, Vec<(&'a str, &'a str)>);

    impl MockTransport {
        fn new(entries: Vec<MockEntry>) -> Self {
            let mut map: HashMap<String, Vec<RawResponse>> = HashMap::new();
            for (u, st, body, hdrs) in entries {
                let mut h = HashMap::new();
                for (k, v) in hdrs {
                    h.insert(k.to_ascii_lowercase(), v.to_string());
                }
                map.entry(u.to_string()).or_default().push(RawResponse {
                    status: st,
                    body: body.as_bytes().to_vec(),
                    headers: h,
                    final_url: u.to_string(),
                });
            }
            Self {
                responses: Mutex::new(map),
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl Transport for MockTransport {
        fn get<'a>(
            &'a self,
            url: &'a str,
            _headers: &'a HashMap<String, String>,
            _timeout: Duration,
        ) -> Pin<Box<dyn Future<Output = Result<RawResponse, String>> + Send + 'a>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::Relaxed);
                let mut guard = self.responses.lock().unwrap();
                let queue = guard.get_mut(url);
                if let Some(q) = queue
                    && !q.is_empty()
                {
                    let item = if q.len() > 1 {
                        q.remove(0)
                    } else {
                        RawResponse {
                            status: q[0].status,
                            body: q[0].body.clone(),
                            headers: q[0].headers.clone(),
                            final_url: q[0].final_url.clone(),
                        }
                    };
                    return Ok(item);
                }
                Ok(RawResponse {
                    status: 404,
                    body: Vec::new(),
                    headers: HashMap::new(),
                    final_url: url.to_string(),
                })
            })
        }
    }

    #[tokio::test]
    async fn enforces_robots_and_conditional_304() {
        let dir = std::env::temp_dir().join(format!("magnet-http-{}", rand::rng().random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("fetch.sqlite");
        let transport = MockTransport::new(vec![
            (
                "https://example.test/robots.txt",
                200,
                "User-agent: *\nDisallow: /private/\nCrawl-delay: 0.05\n",
                vec![],
            ),
            (
                "https://example.test/ok",
                200,
                "hello world",
                vec![("etag", "\"v1\"")],
            ),
            ("https://example.test/ok", 304, "", vec![]),
        ]);
        let fetcher = Fetcher::with_transport(&db, Box::new(transport), 4, 0.0).unwrap();

        let denied = fetcher
            .get("https://example.test/private/x", GetOpts::default())
            .await;
        assert!(denied.robots_skipped);

        let ok1 = fetcher
            .get("https://example.test/ok", GetOpts::default())
            .await;
        assert!(ok1.ok());
        assert_eq!(ok1.text, "hello world");

        // Force expiry of cache entry and verify 304 conditional refresh
        {
            let conn = fetcher.conn.lock().unwrap();
            conn.execute("UPDATE http_cache SET fetched_at = 0", [])
                .unwrap();
        }
        let ok2 = fetcher
            .get(
                "https://example.test/ok",
                GetOpts {
                    conditional: true,
                    ..Default::default()
                },
            )
            .await;
        assert!(ok2.ok());
        assert!(ok2.not_modified);
        assert_eq!(ok2.text, "hello world");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn robots_403_denies_all_and_html_allows() {
        let p403 = RobotsPolicy::parse("Forbidden", 403);
        assert!(!p403.allowed("https://example.test/anything"));

        let phtml = RobotsPolicy::parse("<html><body>Not found</body></html>", 404);
        assert!(phtml.allowed("https://example.test/anything"));
    }
}
