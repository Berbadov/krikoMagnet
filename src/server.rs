use std::collections::HashMap;
use std::fmt::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use axum::extract::{ConnectInfo, DefaultBodyLimit, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;

use crate::learn::today;
use crate::search::Searcher;
use crate::{env_num, log};

const ROUTES: &[&str] = &[
    "/health",
    "/search",
    "/metrics",
    "/cache/clear-queries",
    "/learn",
    "/learn/stats",
];
const MAX_QUERY_CHARS: usize = 512;
const MAX_LEARN_BODY_BYTES: usize = 1024 * 1024;
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

type RequestStats = HashMap<(&'static str, u16), (u64, f64)>;

#[derive(Default)]
struct Metrics {
    requests: Mutex<RequestStats>,
    query_cache_hits: AtomicU64,
    insufficient: AtomicU64,
}

impl Metrics {
    fn record(&self, route: &'static str, status: u16, secs: f64) {
        let mut map = self.requests.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = map.entry((route, status)).or_insert((0, 0.0));
        entry.0 += 1;
        entry.1 += secs;
    }
}

#[derive(Clone)]
struct LogFields(Value);

pub struct AppState {
    pub searcher: Arc<Searcher>,
    pub cfg: ServerConfig,
    limiter: RateLimiter,
    metrics: Metrics,
}

impl AppState {
    pub fn new(searcher: Arc<Searcher>, cfg: ServerConfig) -> Arc<Self> {
        let limiter = RateLimiter::new(cfg.rate_per_min, cfg.rate_burst);
        Arc::new(Self {
            searcher,
            cfg,
            limiter,
            metrics: Metrics::default(),
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

fn route_label(path: &str) -> &'static str {
    ROUTES
        .iter()
        .copied()
        .find(|r| *r == path)
        .unwrap_or("other")
}

async fn observe(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let t0 = Instant::now();
    let method = req.method().to_string();
    let route = route_label(req.uri().path());
    let resp = guard(&st, req, next).await;
    let secs = t0.elapsed().as_secs_f64();
    let status = resp.status().as_u16();
    st.metrics.record(route, status, secs);
    let at = if status >= 500 {
        log::Level::Error
    } else {
        log::Level::Info
    };
    if log::enabled(at) {
        let mut line = json!({
            "event": "request",
            "method": method,
            "route": route,
            "status": status,
            "latency_ms": (secs * 1_000_000.0).round() / 1000.0,
        });
        if let (Some(obj), Some(LogFields(Value::Object(extra)))) =
            (line.as_object_mut(), resp.extensions().get::<LogFields>())
        {
            obj.extend(extra.clone());
        }
        log::emit(at, line);
    }
    resp
}

async fn guard(st: &AppState, req: Request, next: Next) -> Response {
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
    let admin = path.starts_with("/cache/") || path == "/learn" || path.starts_with("/learn/");
    let required = if admin {
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

async fn isolate<F>(fut: F) -> Response
where
    F: Future<Output = Value> + Send + 'static,
{
    match tokio::spawn(fut).await {
        Ok(v) => Json(v).into_response(),
        Err(_) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    }
}

async fn health(State(st): State<Arc<AppState>>, Query(p): Query<HealthParams>) -> Response {
    let s = st.searcher.clone();
    isolate(async move { s.health(p.probe).await }).await
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
    let (s, q) = (st.searcher.clone(), query.to_string());
    let payload =
        match tokio::spawn(async move { s.search(&q, req.k, req.fetch, budget).await }).await {
            Ok(v) => v,
            Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
    let fields = search_fields(&payload, query);
    if payload["query_cache_hit"] == true {
        st.metrics.query_cache_hits.fetch_add(1, Ordering::Relaxed);
    }
    if payload["sufficient"] == false {
        st.metrics.insufficient.fetch_add(1, Ordering::Relaxed);
    }
    let mut resp = Json(payload).into_response();
    resp.extensions_mut().insert(LogFields(fields));
    resp
}

fn search_fields(p: &Value, query: &str) -> Value {
    let results = p["results"].as_array().map_or(&[][..], Vec::as_slice);
    let count = |f: &dyn Fn(&Value) -> bool| results.iter().filter(|r| f(&r["status"])).count();
    let mut fields = json!({
        "query_cache_hit": p["query_cache_hit"],
        "results": results.len(),
        "result_cache_hits": results.iter().filter(|r| r["cache_hit"] == true).count(),
        "sources_tried": p["plan"]["sites"].as_array().map_or(0, Vec::len),
        "sources_with_results": p["plan"]["sources_with_results"],
        "fetches": p["fetch_stats"]["total_fetches"],
        "robots_skips": p["fetch_stats"]["robots_skips"],
        "blocks": count(&|s| {
            matches!(s.as_u64(), Some(401 | 403 | 429))
                || matches!(s.as_str(), Some("robots" | "robots-denied"))
        }),
        "errors": count(&|s| s.as_str() == Some("ERR") || s.as_u64().is_some_and(|c| c >= 500)),
        "sufficient": p["sufficient"],
        "confidence": p["confidence"],
    });
    if log::enabled(log::Level::Debug) {
        fields["query"] = json!(query);
    }
    fields
}

async fn clear_queries(State(st): State<Arc<AppState>>) -> Response {
    st.searcher.cache.clear_queries();
    Json(json!({ "ok": true })).into_response()
}

async fn learn(State(st): State<Arc<AppState>>, body: String) -> Response {
    let Some(store) = st.searcher.learn.clone() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "learning store unavailable",
        );
    };
    match tokio::task::spawn_blocking(move || store.ingest(&body, today())).await {
        Ok(Ok(report)) => Json(report).into_response(),
        _ => error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    }
}

async fn learn_stats(State(st): State<Arc<AppState>>) -> Response {
    let Some(store) = st.searcher.learn.clone() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "learning store unavailable",
        );
    };
    isolate(async move { store.stats() }).await
}

async fn metrics(State(st): State<Arc<AppState>>) -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        render_metrics(&st),
    )
        .into_response()
}

fn render_metrics(st: &AppState) -> String {
    let mut out = String::new();
    let mut put = |kind: &str, name: &str, rows: Vec<(String, String)>| {
        let _ = writeln!(out, "# TYPE {name} {kind}");
        for (labels, value) in rows {
            let _ = writeln!(out, "{name}{labels} {value}");
        }
    };
    let mut reqs: Vec<_> = st
        .metrics
        .requests
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|(k, v)| (*k, *v))
        .collect();
    reqs.sort_by_key(|(k, _)| *k);
    let label = |r: &str, s: u16| format!("{{route=\"{r}\",status=\"{s}\"}}");
    put(
        "counter",
        "magnet_http_requests_total",
        reqs.iter()
            .map(|((r, s), (n, _))| (label(r, *s), n.to_string()))
            .collect(),
    );
    put(
        "counter",
        "magnet_http_request_seconds_total",
        reqs.iter()
            .map(|((r, s), (_, secs))| (label(r, *s), format!("{secs:.6}")))
            .collect(),
    );
    let m = &st.metrics;
    let f = &st.searcher.fetcher.stats;
    for (name, v) in [
        ("magnet_search_query_cache_hits_total", &m.query_cache_hits),
        ("magnet_search_insufficient_total", &m.insufficient),
        ("magnet_fetch_requests_total", &f.requests),
        ("magnet_fetch_cache_hits_total", &f.cache_hits),
        ("magnet_fetch_robots_skips_total", &f.robots_skips),
        ("magnet_fetch_retries_total", &f.retries),
        ("magnet_fetch_bytes_total", &f.bytes),
    ] {
        put(
            "counter",
            name,
            vec![(String::new(), v.load(Ordering::Relaxed).to_string())],
        );
    }
    put(
        "gauge",
        "magnet_uptime_seconds",
        vec![(
            String::new(),
            st.searcher.started.elapsed().as_secs().to_string(),
        )],
    );
    put(
        "gauge",
        "magnet_storage_bytes",
        st.searcher
            .storage_bytes()
            .into_iter()
            .map(|(k, v)| (format!("{{store=\"{k}\"}}"), v.to_string()))
            .collect(),
    );
    let mut circuits: Vec<_> = st.searcher.breaker.snapshot().into_iter().collect();
    circuits.sort_by(|a, b| a.0.cmp(&b.0));
    put(
        "gauge",
        "magnet_circuit_open",
        circuits
            .into_iter()
            .map(|(site, e)| (format!("{{site=\"{site}\"}}"), u8::from(e.open).to_string()))
            .collect(),
    );
    out
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/search", post(search))
        .route("/metrics", get(metrics))
        .route("/cache/clear-queries", post(clear_queries))
        .route(
            "/learn",
            post(learn).layer(DefaultBodyLimit::max(MAX_LEARN_BODY_BYTES)),
        )
        .route("/learn/stats", get(learn_stats))
        .layer(DefaultBodyLimit::max(state.cfg.max_body_bytes))
        .layer(middleware::from_fn_with_state(state.clone(), observe))
        .with_state(state)
}

pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
}

pub async fn run<S>(listener: TcpListener, state: Arc<AppState>, shutdown: S) -> Result<(), String>
where
    S: Future<Output = ()> + Send + 'static,
{
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
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
            learn_path: dir.join("learn.sqlite"),
            ..Default::default()
        };
        (Arc::new(Searcher::open(cfg).unwrap()), dir)
    }

    async fn spawn(cfg: ServerConfig) -> (String, PathBuf) {
        let (searcher, dir) = test_searcher();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(run(
            listener,
            AppState::new(searcher, cfg),
            std::future::pending(),
        ));
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
    async fn learn_routes_need_admin_and_reject_identifying_fields() {
        let c = reqwest::Client::new();
        let (base, dir) = spawn(cfg(Some("secret"), None)).await;
        for (method, path) in [("POST", "/learn"), ("GET", "/learn/stats")] {
            let r = c
                .request(method.parse().unwrap(), format!("{base}{path}"))
                .bearer_auth("secret")
                .send()
                .await
                .unwrap();
            assert_eq!(r.status().as_u16(), 404, "{path}");
        }

        let (base, dir2) = spawn(cfg(Some("secret"), Some("adm"))).await;
        let user = c
            .get(format!("{base}/learn/stats"))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap();
        assert_eq!(user.status().as_u16(), 401);

        let good = json!({
            "product": "acme widget", "kind": "review", "lang": "en", "country": "",
            "url": "https://acme-widgets.com/widget-review", "signal": "useful",
            "count": 3, "day": crate::learn::today().format("%Y-%m-%d").to_string(),
        });
        let mut lines = vec![good.to_string()];
        for field in ["user_id", "device", "ip", "email", "session"] {
            let mut v = good.clone();
            v[field] = json!("someone");
            lines.push(v.to_string());
        }
        let body = lines.join("\n") + &" ".repeat(32 * 1024);
        let report: Value = c
            .post(format!("{base}/learn"))
            .bearer_auth("adm")
            .body(body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(report["accepted"], 1);
        assert_eq!(report["rejected"], 5);
        assert!(!report.to_string().contains("someone"));

        let stats: Value = c
            .get(format!("{base}/learn/stats"))
            .bearer_auth("adm")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(stats["urls"], 1);
        assert_eq!(stats["top_domains"][0]["domain"], "acme-widgets.com");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[tokio::test]
    async fn metrics_and_health_report_observability_fields() {
        let (base, dir) = spawn(cfg(Some("secret"), None)).await;
        let c = reqwest::Client::new();
        let health: Value = c
            .get(format!("{base}/health"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(health["uptime_s"].is_u64());
        assert!(health["storage_bytes"]["fetch_cache"].as_u64().unwrap() > 0);
        assert!(health["circuit"].is_object());

        let denied = c.get(format!("{base}/metrics")).send().await.unwrap();
        assert_eq!(denied.status().as_u16(), 401);
        let text = c
            .get(format!("{base}/metrics"))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(text.contains("magnet_http_requests_total{route=\"/health\",status=\"200\"} 1"));
        assert!(text.contains("magnet_http_requests_total{route=\"/metrics\",status=\"401\"} 1"));
        assert!(text.contains("# TYPE magnet_fetch_requests_total counter"));
        assert!(text.contains("magnet_storage_bytes{store=\"fetch_cache\"}"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_log_fields_count_blocks_and_errors() {
        let payload = json!({
            "query_cache_hit": false,
            "sufficient": false,
            "confidence": 0.2,
            "plan": {"sites": ["a", "b", "c"], "sources_with_results": 2},
            "fetch_stats": {"total_fetches": 7, "robots_skips": 1},
            "results": [
                {"status": 200, "cache_hit": true},
                {"status": 403, "cache_hit": false},
                {"status": "robots", "cache_hit": false},
                {"status": "ERR", "cache_hit": false},
                {"status": 503, "cache_hit": false},
            ],
        });
        let f = search_fields(&payload, "q");
        assert_eq!(f["results"], 5);
        assert_eq!(f["result_cache_hits"], 1);
        assert_eq!(f["sources_tried"], 3);
        assert_eq!(f["blocks"], 2);
        assert_eq!(f["errors"], 2);
        assert_eq!(f["fetches"], 7);
    }

    #[tokio::test]
    async fn graceful_shutdown_returns_cleanly() {
        let (searcher, dir) = test_searcher();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let state = AppState::new(searcher, cfg(None, None));
        let handle = tokio::spawn(run(listener, state, async {
            let _ = rx.await;
        }));
        tx.send(()).unwrap();
        let res = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(matches!(res, Ok(Ok(Ok(())))));
        let _ = std::fs::remove_dir_all(&dir);
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
