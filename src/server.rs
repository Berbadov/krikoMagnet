use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use axum::extract::{ConnectInfo, DefaultBodyLimit, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;

use crate::env_num;
use crate::search::Searcher;

const MAX_QUERY_CHARS: usize = 512;
const MAX_K: usize = 20;
const MAX_BUDGET_MS: u64 = 120_000;
const MAX_RATE_KEYS: usize = 10_000;

pub struct ServerConfig {
    pub api_token: Option<String>,
    pub admin_token: Option<String>,
    pub rate_per_min: f64,
    pub rate_burst: f64,
    pub max_body_bytes: usize,
}

fn env_token(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

impl ServerConfig {
    pub fn from_env() -> Self {
        Self {
            api_token: env_token("MAGNET_API_TOKEN"),
            admin_token: env_token("MAGNET_ADMIN_TOKEN"),
            rate_per_min: env_num("MAGNET_RATE_PER_MIN", 60.0),
            rate_burst: env_num("MAGNET_RATE_BURST", 20.0),
            max_body_bytes: env_num("MAGNET_MAX_BODY_BYTES", 16 * 1024),
        }
    }
}

pub fn token_eq(given: &str, expected: &str) -> bool {
    let a = Sha256::digest(given.as_bytes());
    let b = Sha256::digest(expected.as_bytes());
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn bearer_matches(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| token_eq(t.trim(), expected))
}

pub struct RateLimiter {
    per_sec: f64,
    burst: f64,
    buckets: Mutex<HashMap<IpAddr, (f64, Instant)>>,
}

impl RateLimiter {
    pub fn new(per_min: f64, burst: f64) -> Self {
        Self {
            per_sec: per_min / 60.0,
            burst: burst.max(1.0),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    pub fn check(&self, key: IpAddr, now: Instant) -> Result<(), u64> {
        if self.per_sec <= 0.0 {
            return Ok(());
        }
        let mut map = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        if map.len() >= MAX_RATE_KEYS {
            let refill_s = self.burst / self.per_sec;
            map.retain(|_, (_, at)| now.duration_since(*at).as_secs_f64() < refill_s);
        }
        let (tokens, last) = map.entry(key).or_insert((self.burst, now));
        let elapsed = now.saturating_duration_since(*last).as_secs_f64();
        *tokens = (*tokens + elapsed * self.per_sec).min(self.burst);
        *last = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            Ok(())
        } else {
            Err(((1.0 - *tokens) / self.per_sec).ceil().max(1.0) as u64)
        }
    }
}

pub struct AppState {
    pub searcher: Arc<Searcher>,
    pub cfg: ServerConfig,
    limiter: RateLimiter,
}

impl AppState {
    pub fn new(searcher: Arc<Searcher>, cfg: ServerConfig) -> Arc<Self> {
        let limiter = RateLimiter::new(cfg.rate_per_min, cfg.rate_burst);
        Arc::new(Self {
            searcher,
            cfg,
            limiter,
        })
    }
}

#[derive(Deserialize)]
struct HttpSearchReq {
    query: String,
    #[serde(default = "default_k")]
    k: usize,
    #[serde(default = "default_true")]
    fetch: bool,
    budget_ms: Option<u64>,
}

fn default_k() -> usize {
    3
}
fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
struct HealthParams {
    #[serde(default)]
    probe: bool,
}

fn error_response(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

fn wants_probe(query: Option<&str>) -> bool {
    url::form_urlencoded::parse(query.unwrap_or("").as_bytes())
        .any(|(k, v)| k == "probe" && v != "false")
}

async fn guard(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    if let Err(wait) = st.limiter.check(ip, Instant::now()) {
        let mut resp = error_response(StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded");
        resp.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(wait));
        return resp;
    }
    let path = req.uri().path();
    let required = if path.starts_with("/cache/") {
        match st.cfg.admin_token {
            Some(ref t) => Some(t),
            None => return error_response(StatusCode::NOT_FOUND, "admin routes are disabled"),
        }
    } else if path == "/health" && !wants_probe(req.uri().query()) {
        None
    } else {
        st.cfg.api_token.as_ref()
    };
    if let Some(tok) = required
        && !bearer_matches(req.headers(), tok)
    {
        let mut resp = error_response(StatusCode::UNAUTHORIZED, "unauthorized");
        resp.headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return resp;
    }
    next.run(req).await
}

async fn health(State(st): State<Arc<AppState>>, Query(p): Query<HealthParams>) -> Response {
    Json(st.searcher.health(p.probe).await).into_response()
}

async fn search(State(st): State<Arc<AppState>>, Json(req): Json<HttpSearchReq>) -> Response {
    let query = req.query.trim();
    if query.is_empty() || query.chars().count() > MAX_QUERY_CHARS {
        return error_response(
            StatusCode::BAD_REQUEST,
            &format!("query must be 1..={MAX_QUERY_CHARS} characters"),
        );
    }
    if !(1..=MAX_K).contains(&req.k) {
        return error_response(StatusCode::BAD_REQUEST, &format!("k must be 1..={MAX_K}"));
    }
    let budget = req.budget_ms.map(|b| b.min(MAX_BUDGET_MS));
    Json(st.searcher.search(query, req.k, req.fetch, budget).await).into_response()
}

async fn clear_queries(State(st): State<Arc<AppState>>) -> Response {
    st.searcher.cache.clear_queries();
    Json(json!({ "ok": true })).into_response()
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/search", post(search))
        .route("/cache/clear-queries", post(clear_queries))
        .layer(DefaultBodyLimit::max(state.cfg.max_body_bytes))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

pub async fn run(listener: TcpListener, state: Arc<AppState>) -> Result<(), String> {
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .map_err(|e| format!("serve: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::SearchConfig;
    use std::path::PathBuf;
    use std::time::Duration;

    fn test_searcher() -> (Arc<Searcher>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("magnet-srv-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = SearchConfig {
            registry_path: PathBuf::from("registry.yaml"),
            tranco_path: dir.join("missing_tranco.txt"),
            index_path: None,
            cache_path: dir.join("pipe.sqlite"),
            fetch_cache_path: dir.join("fetch.sqlite"),
            sources_path: dir.join("sources.sqlite"),
            ..Default::default()
        };
        (Arc::new(Searcher::open(cfg).unwrap()), dir)
    }

    async fn spawn(cfg: ServerConfig) -> (String, PathBuf) {
        let (searcher, dir) = test_searcher();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(run(listener, AppState::new(searcher, cfg)));
        (base, dir)
    }

    fn cfg(api: Option<&str>, admin: Option<&str>) -> ServerConfig {
        ServerConfig {
            api_token: api.map(str::to_string),
            admin_token: admin.map(str::to_string),
            rate_per_min: 0.0,
            rate_burst: 1.0,
            max_body_bytes: 1024,
        }
    }

    #[test]
    fn token_compare_and_rate_bucket() {
        assert!(token_eq("abc", "abc"));
        assert!(!token_eq("abc", "abd"));
        assert!(!token_eq("", "abc"));

        let rl = RateLimiter::new(60.0, 2.0);
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let t0 = Instant::now();
        assert!(rl.check(ip, t0).is_ok());
        assert!(rl.check(ip, t0).is_ok());
        assert_eq!(rl.check(ip, t0), Err(1));
        assert!(rl.check(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), t0).is_ok());
        assert!(rl.check(ip, t0 + Duration::from_millis(1100)).is_ok());

        let off = RateLimiter::new(0.0, 1.0);
        for _ in 0..100 {
            assert!(off.check(ip, t0).is_ok());
        }
    }

    #[tokio::test]
    async fn auth_admin_and_body_limits() {
        let (base, dir) = spawn(cfg(Some("secret"), None)).await;
        let c = reqwest::Client::new();
        let status = |r: reqwest::Response| r.status().as_u16();

        assert_eq!(
            status(c.get(format!("{base}/health")).send().await.unwrap()),
            200
        );
        let probe = c
            .get(format!("{base}/health?probe=true"))
            .send()
            .await
            .unwrap();
        assert_eq!(status(probe), 401);
        let body = json!({"query": "x", "k": 0});
        let no_tok = c
            .post(format!("{base}/search"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(status(no_tok), 401);
        let bad_tok = c
            .post(format!("{base}/search"))
            .bearer_auth("nope")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(status(bad_tok), 401);
        let bad_k = c
            .post(format!("{base}/search"))
            .bearer_auth("secret")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(status(bad_k), 400);
        let big = json!({"query": "x".repeat(4096)});
        let too_big = c
            .post(format!("{base}/search"))
            .bearer_auth("secret")
            .json(&big)
            .send()
            .await
            .unwrap();
        assert_eq!(status(too_big), 413);
        let admin_off = c
            .post(format!("{base}/cache/clear-queries"))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap();
        assert_eq!(status(admin_off), 404);

        let (base2, dir2) = spawn(cfg(None, Some("adm"))).await;
        let user = c
            .post(format!("{base2}/cache/clear-queries"))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap();
        assert_eq!(status(user), 401);
        let admin = c
            .post(format!("{base2}/cache/clear-queries"))
            .bearer_auth("adm")
            .send()
            .await
            .unwrap();
        assert_eq!(status(admin), 200);

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[tokio::test]
    async fn rate_limit_returns_429_with_retry_after() {
        let mut c = cfg(None, None);
        c.rate_per_min = 1.0;
        c.rate_burst = 1.0;
        let (base, dir) = spawn(c).await;
        let client = reqwest::Client::new();
        let first = client.get(format!("{base}/health")).send().await.unwrap();
        assert_eq!(first.status().as_u16(), 200);
        let second = client.get(format!("{base}/health")).send().await.unwrap();
        assert_eq!(second.status().as_u16(), 429);
        assert!(second.headers().contains_key("retry-after"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
