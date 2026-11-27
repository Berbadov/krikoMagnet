mod confidence;
mod discover;
mod extract;
mod http;
mod index;
mod log;
mod measure;
mod planner;
mod ranker;
mod scout;
mod search;
mod server;
mod text;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock};

use regex::Regex;

use flate2::read::DeflateDecoder;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Semaphore, mpsc};
use url::Url;

use crate::discover::base_domain;
use crate::http::{Fetcher, USER_AGENT};
use crate::index::{SiteIndex, build_index};
use crate::measure::{
    baseline_default_path, calibrate_from_cache, load_baseline, measure_urls_sequential,
};
use crate::planner::{load_registry, load_tranco};
use crate::scout::{SourceTrustStore, scout_domain};
use crate::search::{
    PipelineCache, PruneConfig, SearchConfig, Searcher, prune_storage, spawn_pruner,
};
use crate::server::{AppState, ServerConfig};

const MCP_INSTRUCTIONS: &str = "Kriko Magnet searches a curated registry of high-value review, complaint, recall and repair sites (plus general reference fallback) without a search-engine provider, then extracts the page text politely (honest user agent, robots.txt, >=1s per host). Use magnet_search for product/buying questions or general lookups; use magnet_fetch to read one page (robots-disallowed URLs are refused); use magnet_health when results look empty or stale.";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run_cli(&args).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    };
    std::process::exit(code);
}

fn env_num<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(default)
}

fn next_val(args: &[String], i: &mut usize, flag: &str) -> Result<String, String> {
    if *i + 1 >= args.len() {
        return Err(format!("missing value for {flag}"));
    }
    *i += 1;
    Ok(args[*i].clone())
}

fn parse_search_cfg(
    args: &[String],
    default_budget_ms: u64,
) -> Result<(SearchConfig, Vec<String>), String> {
    let mut cfg = SearchConfig {
        page_budget_ms: default_budget_ms,
        ..Default::default()
    };
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--registry" => {
                cfg.registry_path = PathBuf::from(next_val(args, &mut i, "--registry")?)
            }
            "--tranco" => cfg.tranco_path = PathBuf::from(next_val(args, &mut i, "--tranco")?),
            "--cache" => cfg.cache_path = PathBuf::from(next_val(args, &mut i, "--cache")?),
            "--fetch-cache" => {
                cfg.fetch_cache_path = PathBuf::from(next_val(args, &mut i, "--fetch-cache")?)
            }
            "--sources" => cfg.sources_path = PathBuf::from(next_val(args, &mut i, "--sources")?),
            "--index" => cfg.index_path = Some(PathBuf::from(next_val(args, &mut i, "--index")?)),
            "--no-index" => cfg.index_path = None,
            "--budget-ms" | "--page-budget" => {
                cfg.page_budget_ms = next_val(args, &mut i, "--budget-ms")?
                    .parse()
                    .map_err(|_| "invalid --budget-ms")?
            }
            _ => rest.push(args[i].clone()),
        }
        i += 1;
    }
    Ok((cfg, rest))
}

async fn run_cli(args: &[String]) -> Result<i32, String> {
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        print_usage();
        return Ok(0);
    }
    let cmd = args[0].as_str();
    let tail = &args[1..];
    match cmd {
        "search" => cmd_search(tail).await,
        "fetch" => cmd_fetch(tail).await,
        "serve" => cmd_serve(tail).await,
        "mcp" => cmd_mcp(tail).await,
        "index" => cmd_index(tail).await,
        "health" => cmd_health(tail).await,
        "calibrate" => cmd_calibrate(tail),
        "measure" => cmd_measure(tail).await,
        "scout" => cmd_scout(tail).await,
        "tranco" => cmd_tranco().await,
        "prune" => cmd_prune(tail),
        _ => Err(format!("unknown subcommand '{cmd}' (run --help)")),
    }
}

fn print_usage() {
    println!(
        "magnet v0.3.0 — engine-free, polite search\n\n\
         USAGE:\n  \
           magnet search <query> [-k 5] [--no-fetch] [--budget-ms MS] [--no-index]\n  \
           magnet fetch <url>\n  \
           magnet serve [--host 127.0.0.1] [--port 8000] [--budget-ms MS] [--refresh-mins N]\n  \
           magnet mcp [--budget-ms 15000]\n  \
           magnet index <build|refresh|stats> [--site ID ...] [--recent 12] [--concurrency 4] [--db PATH]\n  \
           magnet health [--probe]\n  \
           magnet calibrate [--fetch-cache PATH] [--out PATH]\n  \
           magnet measure <url...> [--baseline PATH]\n  \
           magnet scout <domain...> [--file PATH] [--sample 8] [--promote [--apply [--build-index]]] [--json]\n  \
           magnet tranco\n  \
           magnet prune [--vacuum] [--cache PATH] [--fetch-cache PATH]"
    );
}

fn cmd_prune(args: &[String]) -> Result<i32, String> {
    let (cfg, rest) = parse_search_cfg(args, 0)?;
    let mut vacuum = false;
    for a in &rest {
        match a.as_str() {
            "--vacuum" => vacuum = true,
            s => return Err(format!("unknown flag {s}")),
        }
    }
    let fetcher = Fetcher::open(&cfg.fetch_cache_path)?;
    let cache = PipelineCache::open(&cfg.cache_path)?;
    let report = prune_storage(&fetcher, &cache, &PruneConfig::from_env(), vacuum);
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    Ok(0)
}

async fn cmd_search(args: &[String]) -> Result<i32, String> {
    let (cfg, rest) = parse_search_cfg(args, 0)?;
    let mut k = 5usize;
    let mut fetch_pages = true;
    let mut query_parts = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "-k" | "--k" => {
                k = next_val(&rest, &mut i, "-k")?
                    .parse()
                    .map_err(|_| "invalid -k")?
            }
            "--no-fetch" => fetch_pages = false,
            s => query_parts.push(s.to_string()),
        }
        i += 1;
    }
    let query = query_parts.join(" ").trim().to_string();
    if query.is_empty() {
        return Err("missing <query>".into());
    }
    let searcher = Searcher::open(cfg)?;
    let out = searcher.search(&query, k, fetch_pages, None).await;
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    Ok(0)
}

async fn cmd_fetch(args: &[String]) -> Result<i32, String> {
    let (cfg, rest) = parse_search_cfg(args, 0)?;
    let url = rest.first().ok_or("missing <url>")?;
    let searcher = Searcher::open(cfg)?;
    let out = searcher.fetch_one_page(url).await;
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    Ok(if out["ok"].as_bool().unwrap_or(false) {
        0
    } else {
        1
    })
}

async fn cmd_serve(args: &[String]) -> Result<i32, String> {
    let env_budget = std::env::var("KRIKO_PAGE_BUDGET_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let (cfg, rest) = parse_search_cfg(args, env_budget)?;
    let mut host = std::env::var("MAGNET_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let mut port: u16 = env_num("MAGNET_PORT", 8000);
    let mut refresh_mins: f64 = std::env::var("KRIKO_REFRESH_MINUTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--host" => host = next_val(&rest, &mut i, "--host")?,
            "--port" => {
                port = next_val(&rest, &mut i, "--port")?
                    .parse()
                    .map_err(|_| "invalid --port")?
            }
            "--refresh-mins" => {
                refresh_mins = next_val(&rest, &mut i, "--refresh-mins")?
                    .parse()
                    .map_err(|_| "invalid --refresh-mins")?
            }
            s => return Err(format!("unknown flag {s}")),
        }
        i += 1;
    }

    let reg_path = cfg.registry_path.clone();
    let idx_path = cfg.index_path.clone();
    let searcher = Arc::new(Searcher::open(cfg)?);
    spawn_pruner(searcher.clone(), PruneConfig::from_env());

    if refresh_mins > 0.0
        && let Some(db) = idx_path
    {
        tokio::spawn(async move {
            let wait = std::time::Duration::from_secs_f64(refresh_mins * 60.0);
            loop {
                tokio::time::sleep(wait).await;
                if let Ok(reg) = load_registry(&reg_path) {
                    let _ = build_index(&reg, &[], &db, false, 12, 4).await;
                }
            }
        });
    }

    let server_cfg = ServerConfig::from_env();
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("bind {addr}: {e}"))?;
    let bound = listener
        .local_addr()
        .map_err(|e| format!("local_addr: {e}"))?;
    if !bound.ip().is_loopback() && server_cfg.api_token.is_none() {
        eprintln!("warning: listening on {bound} without MAGNET_API_TOKEN; /search is open");
    }
    eprintln!("magnet listening on http://{bound}");
    server::run(
        listener,
        AppState::new(searcher, server_cfg),
        server::shutdown_signal(),
    )
    .await?;
    eprintln!("magnet stopped");
    Ok(0)
}

async fn cmd_index(args: &[String]) -> Result<i32, String> {
    let sub = args.first().map(String::as_str).unwrap_or("stats");
    let mut db = SiteIndex::default_path();
    let mut reg_path = PathBuf::from("registry.yaml");
    let mut sites = Vec::new();
    let mut recent = 12usize;
    let mut concurrency = 4usize;
    let mut as_json = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--db" => db = PathBuf::from(next_val(args, &mut i, "--db")?),
            "--registry" => reg_path = PathBuf::from(next_val(args, &mut i, "--registry")?),
            "--site" => sites.push(next_val(args, &mut i, "--site")?),
            "--recent" => {
                recent = next_val(args, &mut i, "--recent")?
                    .parse()
                    .map_err(|_| "invalid --recent")?
            }
            "--concurrency" => {
                concurrency = next_val(args, &mut i, "--concurrency")?
                    .parse()
                    .map_err(|_| "invalid --concurrency")?
            }
            "--json" => as_json = true,
            s => return Err(format!("unknown flag {s}")),
        }
        i += 1;
    }

    match sub {
        "stats" => {
            let idx = SiteIndex::open(&db)?;
            println!("{}", serde_json::to_string_pretty(&idx.stats()).unwrap());
            Ok(0)
        }
        "build" | "refresh" => {
            let reg = load_registry(&reg_path)?;
            let full = sub == "build";
            let (reports, stats) =
                build_index(&reg, &sites, &db, full, recent, concurrency).await?;
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "mode": sub, "sites": reports, "index": stats
                    }))
                    .unwrap()
                );
            } else {
                for r in &reports {
                    println!(
                        "{:<16} pages={:<6} feeds={:<4} robots={:<2} {:>5.1}s {}",
                        r.site,
                        r.pages,
                        r.feeds,
                        r.robots_skips,
                        r.seconds,
                        r.errors.join("; ")
                    );
                }
                println!("{}", serde_json::to_string(&stats).unwrap());
            }
            Ok(0)
        }
        _ => Err(format!("unknown index subcommand '{sub}'")),
    }
}

async fn cmd_health(args: &[String]) -> Result<i32, String> {
    let (cfg, rest) = parse_search_cfg(args, 0)?;
    let probe = rest.iter().any(|a| a == "--probe");
    let searcher = Searcher::open(cfg)?;
    let report = searcher.health(probe).await;
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    let ok = report["ok"].as_bool().unwrap_or(false)
        && report["circuit_open"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true);
    Ok(if ok { 0 } else { 1 })
}

fn cmd_calibrate(args: &[String]) -> Result<i32, String> {
    let mut reg_path = PathBuf::from("registry.yaml");
    let mut cache_path = Fetcher::default_cache_path();
    let mut out_path = baseline_default_path();
    let mut as_json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--registry" => reg_path = PathBuf::from(next_val(args, &mut i, "--registry")?),
            "--fetch-cache" => cache_path = PathBuf::from(next_val(args, &mut i, "--fetch-cache")?),
            "--out" => out_path = PathBuf::from(next_val(args, &mut i, "--out")?),
            "--json" => as_json = true,
            s => return Err(format!("unknown flag {s}")),
        }
        i += 1;
    }
    let reg = load_registry(&reg_path)?;
    let (baseline, estimates) = calibrate_from_cache(&cache_path, &reg, &out_path)?;
    if let Ok(ts) = SourceTrustStore::open(&SourceTrustStore::default_path()) {
        for e in &estimates {
            let sid = reg
                .sites
                .iter()
                .find(|s| base_domain(&s.domain) == base_domain(&e.host))
                .map(|s| s.id.as_str());
            ts.record_estimate(sid, e);
        }
    }
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "baseline": baseline,
                "hosts": estimates,
            }))
            .unwrap()
        );
    } else {
        println!(
            "calibrated {} golden hosts ({} pages) -> {}",
            baseline.sites_calibrated,
            baseline.pages_calibrated,
            out_path.display()
        );
        println!(
            "{:<28} {:>3} {:>6} {:>15} {:>6} {:>6} {:>6} {:>6} {:>8}",
            "host", "n", "p_gold", "[low, high]", "burst", "zlib", "dup", "aff%", "decision"
        );
        for e in &estimates {
            println!(
                "{:<28} {:>3} {:>6.3} [{:.2}, {:.2}]   {:>6.3} {:>6.3} {:>6.3} {:>5.1}% {:>8}",
                e.host,
                e.pages_sampled,
                e.p_golden,
                e.p_low,
                e.p_high,
                e.shape.median_burstiness,
                e.shape.median_zlib_ratio,
                e.shape.cross_page_dup,
                e.shape.median_affiliate_ratio * 100.0,
                e.decision
            );
        }
    }
    Ok(0)
}

async fn cmd_measure(args: &[String]) -> Result<i32, String> {
    let (cfg, rest) = parse_search_cfg(args, 0)?;
    let mut baseline_path = baseline_default_path();
    let mut urls = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--baseline" => baseline_path = PathBuf::from(next_val(&rest, &mut i, "--baseline")?),
            s if s.starts_with("http") => urls.push(s.to_string()),
            s => return Err(format!("expected http(s) URL or flag, got '{s}'")),
        }
        i += 1;
    }
    if urls.is_empty() {
        return Err("missing <url...>".into());
    }
    let host = Url::parse(&urls[0])
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_else(|| base_domain(&urls[0]));
    let fetcher = Fetcher::open(&cfg.fetch_cache_path)?;
    let baseline = load_baseline(&baseline_path);
    let est = measure_urls_sequential(&host, &urls, &fetcher, &baseline, -0.5).await;
    println!("{}", serde_json::to_string_pretty(&est).unwrap());
    Ok(0)
}

fn scout_target(registry: &Path, apply: bool) -> Result<PathBuf, String> {
    if apply {
        return Ok(registry.to_path_buf());
    }
    let pending = registry.with_extension("pending.yaml");
    if !pending.exists() {
        std::fs::copy(registry, &pending)
            .map_err(|e| format!("create {}: {e}", pending.display()))?;
    }
    Ok(pending)
}

async fn cmd_scout(args: &[String]) -> Result<i32, String> {
    let (cfg, rest) = parse_search_cfg(args, 0)?;
    let mut baseline_path = baseline_default_path();
    let mut sample_limit = 8usize;
    let mut promote = false;
    let mut apply = false;
    let mut do_index = false;
    let mut as_json = false;
    let mut targets = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--baseline" => baseline_path = PathBuf::from(next_val(&rest, &mut i, "--baseline")?),
            "--sample" => {
                sample_limit = next_val(&rest, &mut i, "--sample")?
                    .parse()
                    .map_err(|_| "invalid --sample")?;
            }
            "--file" => {
                let fp = next_val(&rest, &mut i, "--file")?;
                let txt = std::fs::read_to_string(&fp).map_err(|e| format!("read {fp}: {e}"))?;
                for line in txt.lines() {
                    let t = line.trim();
                    if !t.is_empty() && !t.starts_with('#') {
                        targets.push(t.to_string());
                    }
                }
            }
            "--promote" => promote = true,
            "--apply" => apply = true,
            "--build-index" => do_index = true,
            "--json" => as_json = true,
            s if s.starts_with('-') => return Err(format!("unknown flag {s}")),
            s => targets.push(s.to_string()),
        }
        i += 1;
    }
    if targets.is_empty() {
        return Err("missing <domain...> or --file <path>".into());
    }
    if apply && !promote {
        return Err("--apply requires --promote".into());
    }
    if do_index && !apply {
        return Err("--build-index requires --promote --apply".into());
    }

    let target_path = if promote {
        scout_target(&cfg.registry_path, apply)?
    } else {
        cfg.registry_path.clone()
    };
    let mut reg = load_registry(&target_path)?;
    let tranco = load_tranco(&cfg.tranco_path);
    let baseline = load_baseline(&baseline_path);
    let fetcher = Fetcher::open(&cfg.fetch_cache_path)?;
    let trust_store = SourceTrustStore::open(&cfg.sources_path).ok();

    let mut reports = Vec::new();
    let mut promoted_ids = Vec::new();
    for target in targets {
        let promote_opt = promote.then_some(target_path.as_path());
        let mut rep = scout_domain(
            &target,
            &reg,
            &tranco,
            &fetcher,
            &baseline,
            trust_store.as_ref(),
            sample_limit,
            promote_opt,
        )
        .await;
        if rep.action == "promoted" {
            promoted_ids.push(rep.site_id.clone());
            if let Ok(updated) = load_registry(&target_path) {
                reg = updated;
            }
        }
        if !apply {
            match rep.action.as_str() {
                "promoted" => rep.action = "proposed".into(),
                "skipped" => rep.action = "proposed-skip".into(),
                _ => {}
            }
        }
        reports.push(rep);
    }
    if promote && !apply {
        eprintln!(
            "scout proposals are in {}; review with `diff {} {}`, then copy it over or rerun with --apply",
            target_path.display(),
            cfg.registry_path.display(),
            target_path.display()
        );
    }

    if do_index
        && !promoted_ids.is_empty()
        && let Some(ref db) = cfg.index_path
    {
        let _ = build_index(&reg, &promoted_ids, db, true, 6, 4).await;
    }

    if as_json {
        println!("{}", serde_json::to_string_pretty(&reports).unwrap());
    } else {
        println!(
            "{:<28} {:<10} {:>3} {:>6} {:>15} {:>6} {:>6} {:>10}  note",
            "domain", "method", "n", "p_gold", "[low, high]", "burst", "dup", "action"
        );
        for r in &reports {
            println!(
                "{:<28} {:<10} {:>3} {:>6.3} [{:.2}, {:.2}]   {:>6.3} {:>6.3} {:>10}  {}",
                r.domain,
                r.discovered_method,
                r.estimate.pages_sampled,
                r.estimate.p_golden,
                r.estimate.p_low,
                r.estimate.p_high,
                r.estimate.shape.median_burstiness,
                r.estimate.shape.cross_page_dup,
                r.action,
                r.reason
            );
        }
    }
    Ok(0)
}

async fn cmd_tranco() -> Result<i32, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("http: {e}"))?;
    let url = "https://tranco-list.s3.amazonaws.com/top-1m.csv.zip";
    let resp = client
        .get(url)
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(|e| format!("tranco fetch: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("tranco HTTP {}", resp.status()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("tranco bytes: {e}"))?;
    let csv_txt = unzip_first_entry(&bytes)?;
    let mut domains = Vec::with_capacity(100_000);
    for line in csv_txt.lines() {
        if let Some((_, dom)) = line.split_once(',') {
            let d = dom.trim();
            if !d.is_empty() {
                domains.push(d);
                if domains.len() >= 100_000 {
                    break;
                }
            }
        }
    }
    std::fs::create_dir_all("data").map_err(|e| format!("mkdir data: {e}"))?;
    let out_path = PathBuf::from("data").join("tranco_top100k.txt");
    std::fs::write(&out_path, domains.join("\n")).map_err(|e| format!("write tranco: {e}"))?;
    println!("cached {} domains -> {}", domains.len(), out_path.display());
    Ok(0)
}

fn unzip_first_entry(zip: &[u8]) -> Result<String, String> {
    // Minimal PK\x03\x04 local file header reader (handles deflate & data descriptor via CD lookup).
    let eocd = zip
        .windows(4)
        .rposition(|w| w == b"PK\x05\x06")
        .ok_or("invalid zip: missing EOCD")?;
    let cd_offset = u32::from_le_bytes(zip[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    if zip.get(cd_offset..cd_offset + 4) != Some(b"PK\x01\x02") {
        return Err("invalid zip: missing central dir".into());
    }
    let method = u16::from_le_bytes(zip[cd_offset + 10..cd_offset + 12].try_into().unwrap());
    let comp_sz =
        u32::from_le_bytes(zip[cd_offset + 20..cd_offset + 24].try_into().unwrap()) as usize;
    let lfh_off =
        u32::from_le_bytes(zip[cd_offset + 42..cd_offset + 46].try_into().unwrap()) as usize;
    if zip.get(lfh_off..lfh_off + 4) != Some(b"PK\x03\x04") {
        return Err("invalid zip: missing local header".into());
    }
    let name_len = u16::from_le_bytes(zip[lfh_off + 26..lfh_off + 28].try_into().unwrap()) as usize;
    let extra_len =
        u16::from_le_bytes(zip[lfh_off + 28..lfh_off + 30].try_into().unwrap()) as usize;
    let data_start = lfh_off + 30 + name_len + extra_len;
    let slice = zip
        .get(data_start..data_start + comp_sz)
        .ok_or("invalid zip bounds")?;
    let mut text = String::new();
    match method {
        0 => text = String::from_utf8_lossy(slice).into_owned(),
        8 => {
            let mut dec = DeflateDecoder::new(slice);
            dec.read_to_string(&mut text)
                .map_err(|e| format!("inflate: {e}"))?;
        }
        m => return Err(format!("unsupported zip compression {m}")),
    }
    Ok(text)
}

// --- Stdio MCP server ----------------------------------------------------

async fn cmd_mcp(args: &[String]) -> Result<i32, String> {
    let (cfg, _) = parse_search_cfg(args, 15_000)?;
    let searcher = Arc::new(Searcher::open(cfg)?);
    spawn_pruner(searcher.clone(), PruneConfig::from_env());
    let slots = Arc::new(Semaphore::new(
        env_num("MAGNET_MCP_CONCURRENCY", 4usize).max(1),
    ));
    run_mcp_loop(tokio::io::stdin(), tokio::io::stdout(), searcher, slots).await;
    Ok(0)
}

async fn run_mcp_loop<R, W>(input: R, mut output: W, searcher: Arc<Searcher>, slots: Arc<Semaphore>)
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if output.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            let _ = output.flush().await;
        }
    });
    let mut lines = BufReader::new(input).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(req) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let Some(id) = req.get("id").cloned() else {
            continue; // notification
        };
        let (s, sl, tx) = (searcher.clone(), slots.clone(), tx.clone());
        tokio::spawn(async move {
            let inner_id = id.clone();
            let method = req["method"].as_str().unwrap_or("").to_string();
            let inner = tokio::spawn(async move {
                handle_mcp_request(inner_id, &method, &req["params"], &s, &sl).await
            });
            let resp = inner.await.unwrap_or_else(|_| {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32603, "message": "internal error" }
                })
            });
            let _ = tx.send(format!("{resp}\n"));
        });
    }
    drop(tx);
    let _ = writer.await;
}

async fn handle_mcp_request(
    id: Value,
    method: &str,
    params: &Value,
    searcher: &Arc<Searcher>,
    search_slots: &Arc<Semaphore>,
) -> Value {
    match method {
        "initialize" => {
            let pv = params["protocolVersion"].as_str().unwrap_or("2025-03-26");
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": pv,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "magnet", "version": "0.3.0" },
                    "instructions": MCP_INSTRUCTIONS,
                }
            })
        }
        "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "tools": mcp_tool_defs() }
        }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            let args = &params["arguments"];
            let res = match name {
                "magnet_search" => mcp_search(args, searcher, search_slots).await,
                "magnet_fetch" => mcp_fetch(args, searcher).await,
                "magnet_health" => mcp_health(args, searcher).await,
                other => Err(format!("Unknown tool: {other}")),
            };
            let payload = match res {
                Ok(val) => json!({
                    "content": [{ "type": "text", "text": serde_json::to_string(&val).unwrap() }],
                    "isError": false
                }),
                Err(msg) => json!({
                    "content": [{ "type": "text", "text": msg }],
                    "isError": true
                }),
            };
            json!({ "jsonrpc": "2.0", "id": id, "result": payload })
        }
        other => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("Method not found: {other}") }
        }),
    }
}

fn mcp_tool_defs() -> Value {
    json!([
        {
            "name": "magnet_search",
            "description": "Rank pages for a query from the Kriko Magnet site registry.",
            "inputSchema": {
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": { "type": "string", "description": "Natural-language search query." },
                    "k": { "type": "integer", "default": 5, "minimum": 1, "maximum": 10 },
                    "lang": { "type": ["string", "null"], "enum": ["en", "tr", null] },
                    "fetch": { "type": "boolean", "default": true }
                }
            }
        },
        {
            "name": "magnet_fetch",
            "description": "Fetch one URL through the polite layer and return its extracted main text.",
            "inputSchema": {
                "type": "object",
                "required": ["url"],
                "properties": {
                    "url": { "type": "string", "description": "Absolute http(s) URL to fetch." }
                }
            }
        },
        {
            "name": "magnet_health",
            "description": "Adapter health report: registry, local index, in-process ranker, open circuits.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "probe": { "type": "boolean", "default": false }
                }
            }
        }
    ])
}

async fn mcp_search(
    args: &Value,
    searcher: &Arc<Searcher>,
    slots: &Arc<Semaphore>,
) -> Result<Value, String> {
    let query = args["query"].as_str().unwrap_or("").trim().to_string();
    if query.is_empty() {
        return Err("query must be a non-empty string".into());
    }
    let k = match args.get("k") {
        None | Some(Value::Null) => 5usize,
        Some(v) => {
            let n = v.as_i64().ok_or("k must be an integer from 1 to 10")?;
            if !(1..=10).contains(&n) {
                return Err("k must be an integer from 1 to 10".into());
            }
            n as usize
        }
    };
    let lang = match args.get("lang") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let l = v.as_str().ok_or("lang must be 'en' or 'tr'")?;
            if l != "en" && l != "tr" {
                return Err("lang must be 'en' or 'tr'".into());
            }
            Some(l.to_string())
        }
    };
    let fetch = args.get("fetch").and_then(|v| v.as_bool()).unwrap_or(true);

    let _permit = slots
        .acquire()
        .await
        .map_err(|_| "server is shutting down")?;
    let st = &searcher.fetcher.stats;
    let b_req = st.requests.load(Ordering::Relaxed);
    let b_bytes = st.bytes.load(Ordering::Relaxed);
    let b_hits = st.cache_hits.load(Ordering::Relaxed);
    let b_rob = st.robots_skips.load(Ordering::Relaxed);
    let b_ret = st.retries.load(Ordering::Relaxed);

    let payload = searcher.search(&query, k, fetch, None).await;
    let mut rows = Vec::new();
    for (idx, item) in payload["results"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let u = item["url"].as_str().unwrap_or("");
        let text = item["clean_markdown"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| item["snippet"].as_str())
            .unwrap_or("");
        rows.push(json!({
            "rank": idx + 1,
            "title": clip_ws(item["title"].as_str().unwrap_or(""), 160),
            "url": u,
            "site": base_domain(u),
            "snippet": clip_ws(&clean_snippet(text), 280),
            "score": (item["rank_score"].as_f64().unwrap_or(0.0) * 10000.0).round() / 10000.0,
            "cached": item["cache_hit"].as_bool().unwrap_or(false),
            "fetched": item["status"].as_u64() == Some(200),
            "status": item["status"],
            "source": item["source"],
        }));
    }

    if let Some(ref want_lang) = lang {
        let filtered: Vec<Value> = rows
            .iter()
            .filter(|r| {
                let s = r["site"].as_str().unwrap_or("");
                searcher.site_languages.get(s).map(String::as_str) == Some(want_lang.as_str())
            })
            .cloned()
            .collect();
        if !filtered.is_empty() {
            rows = filtered;
        }
    }

    let mut results = Vec::new();
    let mut skipped = Vec::new();
    for row in rows {
        let st_v = &row["status"];
        if matches!(st_v.as_str(), Some("robots" | "robots-denied")) {
            skipped.push(json!({ "url": row["url"], "reason": "robots.txt disallows this path" }));
        } else if let Some(code) = st_v.as_u64()
            && code >= 400
        {
            skipped.push(json!({ "url": row["url"], "reason": format!("HTTP {code}") }));
        } else if st_v.as_str() == Some("ERR") {
            skipped.push(json!({ "url": row["url"], "reason": "fetch error" }));
        } else {
            results.push(row);
        }
    }
    results.truncate(k);
    for (i, r) in results.iter_mut().enumerate() {
        r["rank"] = json!(i + 1);
    }

    Ok(json!({
        "query": query,
        "k": k,
        "lang": lang,
        "pages_fetched": fetch,
        "elapsed_ms": payload["elapsed_ms"],
        "sufficient": payload["sufficient"],
        "confidence": payload["confidence"],
        "reason": payload["reason"],
        "plan": {
            "language": payload["plan"]["language"],
            "topics": payload["plan"]["topics"],
            "sites": payload["plan"]["sites"],
        },
        "polite": {
            "http_requests": st.requests.load(Ordering::Relaxed) - b_req,
            "bytes": st.bytes.load(Ordering::Relaxed) - b_bytes,
            "cache_hits": st.cache_hits.load(Ordering::Relaxed) - b_hits,
            "robots_skips": st.robots_skips.load(Ordering::Relaxed) - b_rob,
            "retries": st.retries.load(Ordering::Relaxed) - b_ret,
            "min_host_interval_s": searcher.fetcher.min_interval_s,
            "user_agent": USER_AGENT,
        },
        "results": results,
        "skipped": skipped,
    }))
}

async fn mcp_fetch(args: &Value, searcher: &Arc<Searcher>) -> Result<Value, String> {
    let raw = args["url"].as_str().unwrap_or("").trim();
    let parsed = Url::parse(raw).map_err(|_| "url must be an absolute http(s) URL".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("url must be an absolute http(s) URL".into());
    }
    Ok(searcher.fetch_one_page(raw).await)
}

async fn mcp_health(args: &Value, searcher: &Arc<Searcher>) -> Result<Value, String> {
    let probe = args.get("probe").and_then(|v| v.as_bool()).unwrap_or(false);
    Ok(searcher.health(probe).await)
}

static MD_IMG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!\[[^\]]*\]\([^)]*\)").expect("valid md image regex"));
static MD_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\([^)]*\)").expect("valid md link regex"));
static MD_HEADING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*#{1,6}\s+").expect("valid md heading regex"));

fn clean_snippet(text: &str) -> String {
    let no_img = MD_IMG_RE.replace_all(text, " ");
    let flat_links = MD_LINK_RE.replace_all(&no_img, "$1");
    let no_headings = MD_HEADING_RE.replace_all(&flat_links, "");
    no_headings.replace("**", "").replace("__", "")
}

fn clip_ws(text: &str, limit: usize) -> String {
    let clean = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = clean.chars().count();
    if count <= limit {
        clean
    } else {
        let mut s: String = clean.chars().take(limit.saturating_sub(1)).collect();
        while s.ends_with(' ') {
            s.pop();
        }
        s.push('…');
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;

    #[tokio::test]
    async fn mcp_protocol_initialize_list_health_and_validation() {
        let dir = std::env::temp_dir().join(format!("magnet-mcp-{}", rand::rng().random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = SearchConfig {
            registry_path: PathBuf::from("registry.yaml"),
            tranco_path: dir.join("missing_tranco.txt"),
            index_path: None,
            cache_path: dir.join("pipe.sqlite"),
            fetch_cache_path: dir.join("fetch.sqlite"),
            ..Default::default()
        };
        let searcher = Arc::new(Searcher::open(cfg).unwrap());
        let lock = Arc::new(Semaphore::new(1));

        let init = handle_mcp_request(json!(1), "initialize", &json!({}), &searcher, &lock).await;
        assert_eq!(init["result"]["serverInfo"]["name"], "magnet");

        let list = handle_mcp_request(json!(2), "tools/list", &json!({}), &searcher, &lock).await;
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 3);

        let health = handle_mcp_request(
            json!(3),
            "tools/call",
            &json!({"name": "magnet_health", "arguments": {}}),
            &searcher,
            &lock,
        )
        .await;
        assert_eq!(health["result"]["isError"], false);

        let bad = handle_mcp_request(
            json!(4),
            "tools/call",
            &json!({"name": "magnet_fetch", "arguments": {"url": "ftp://x.com"}}),
            &searcher,
            &lock,
        )
        .await;
        assert_eq!(bad["result"]["isError"], true);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn mcp_loop_answers_every_request_and_exits_on_eof() {
        let dir = std::env::temp_dir().join(format!("magnet-mcp-{}", rand::rng().random::<u64>()));
        let cfg = SearchConfig {
            registry_path: PathBuf::from("registry.yaml"),
            tranco_path: dir.join("missing_tranco.txt"),
            index_path: None,
            cache_path: dir.join("pipe.sqlite"),
            fetch_cache_path: dir.join("fetch.sqlite"),
            sources_path: dir.join("sources.sqlite"),
            ..Default::default()
        };
        let searcher = Arc::new(Searcher::open(cfg).unwrap());
        let input: &'static [u8] = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n\
            {\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n\
            not json\n\
            {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n\
            {\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"magnet_search\",\"arguments\":{\"query\":\" \"}}}\n";
        let (mut client, server_side) = tokio::io::duplex(1 << 16);
        run_mcp_loop(input, server_side, searcher, Arc::new(Semaphore::new(2))).await;
        let mut out = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut client, &mut out)
            .await
            .unwrap();
        let resps: Vec<Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let mut ids: Vec<i64> = resps.iter().map(|r| r["id"].as_i64().unwrap()).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2, 3]);
        let search = resps.iter().find(|r| r["id"] == 3).unwrap();
        assert_eq!(search["result"]["isError"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn scout_stages_changes_unless_applied() {
        let dir =
            std::env::temp_dir().join(format!("magnet-scout-{}", rand::rng().random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let reg = dir.join("registry.yaml");
        std::fs::copy("registry.yaml", &reg).unwrap();
        let before = std::fs::read_to_string(&reg).unwrap();

        let pending = scout_target(&reg, false).unwrap();
        assert_eq!(pending, dir.join("registry.pending.yaml"));
        scout::append_skipped_yaml(&pending, "staged.example", "test").unwrap();
        assert_eq!(scout_target(&reg, false).unwrap(), pending);
        let staged = load_registry(&pending).unwrap();
        assert!(staged.skipped.iter().any(|s| s.domain == "staged.example"));
        assert_eq!(std::fs::read_to_string(&reg).unwrap(), before);
        assert_eq!(scout_target(&reg, true).unwrap(), reg);

        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let err = cmd_scout(&args(&["a.example", "--apply"]))
            .await
            .unwrap_err();
        assert!(err.contains("--apply requires --promote"));
        let err = cmd_scout(&args(&["a.example", "--promote", "--build-index"]))
            .await
            .unwrap_err();
        assert!(err.contains("--build-index requires"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clean_snippet_strips_markdown_images_and_links() {
        let raw = "![Image 1: hero](https://cdn.example.com/tv.jpg) ### Verdict The **[LG C1 OLED](https://example.com/lg)** remains great.";
        let out = clip_ws(&clean_snippet(raw), 280);
        assert_eq!(out, "Verdict The LG C1 OLED remains great.");
    }
}
