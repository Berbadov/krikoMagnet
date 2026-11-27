use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use regex::Regex;
use rusqlite::Connection;
use serde_json::{Value, json};

const REGISTRY: &str = "version: 1
sites:
  - id: acme
    name: Acme
    domain: acme-widgets.com
    topics: [general]
    language: en
    methods:
      - type: url_pattern
        max_query_terms: 0
";
const QUERY: &str = "acme widget review";
const SEEN: &str = "acme-widgets.com/widget-review";
const BELOW_K: &str = "acme-widgets.com/rare-page";
const PREFER: &str = "hintprefer-zq.com";
const AVOID: &str = "hintavoid-zq.net";
const IDENTIFYING: &str =
    r"(?i)user|device|ip|session|email|phone|account|token|cookie|uid|install|ad_id|advert";
const SAFE_COLUMNS: &[&str] = &["snippet"];
const DATABASES: &[&str] = &[
    "fetch_cache.sqlite",
    "pipeline_cache.sqlite",
    "sources.sqlite",
    "site_index.sqlite",
    "learn.sqlite",
];

fn magnet(dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_magnet"));
    cmd.current_dir(dir);
    for (key, _) in std::env::vars() {
        if key.starts_with("MAGNET_") || key.starts_with("KRIKO_") {
            cmd.env_remove(key);
        }
    }
    cmd.env("MAGNET_LOG", "debug")
        .env("MAGNET_ADMIN_TOKEN", "adm")
        .env("MAGNET_PRUNE_MINS", "0");
    cmd
}

fn run(dir: &Path, args: &[&str]) -> Output {
    let out = magnet(dir).args(args).output().unwrap();
    assert!(out.status.success(), "{args:?}: {out:?}");
    out
}

fn line(url: &str, count: u32, extra: Option<&str>) -> String {
    let mut v = json!({
        "product": "Acme Widget", "kind": "review", "lang": "en", "country": "TR",
        "url": url, "signal": "useful", "count": count,
        "day": chrono::Utc::now().format("%Y-%m-%d").to_string(),
    });
    if let Some(field) = extra {
        v[field] = json!("someone@example.com");
    }
    v.to_string()
}

fn identifying_lines() -> Vec<String> {
    ["user_id", "device", "ip", "email", "session"]
        .iter()
        .map(|f| line(&format!("https://{SEEN}"), 50, Some(f)))
        .collect()
}

fn spawn_server(dir: &Path) -> (Child, String, JoinHandle<Vec<String>>) {
    let mut child = magnet(dir)
        .args(["serve", "--port", "0"])
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut lines = Vec::new();
        for l in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(addr) = l.strip_prefix("magnet listening on ") {
                let _ = tx.send(addr.to_string());
            }
            lines.push(l);
        }
        lines
    });
    let base = rx.recv_timeout(Duration::from_secs(60)).unwrap();
    (child, base, reader)
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        if p.is_dir() {
            out.extend(files_under(&p));
        } else {
            out.push(p);
        }
    }
    out
}

fn identifying_names(db: &Path) -> Vec<String> {
    let re = Regex::new(IDENTIFYING).unwrap();
    let conn = Connection::open(db).unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .flatten()
        .collect();
    assert!(!tables.is_empty(), "{} has no tables", db.display());
    let mut bad = Vec::new();
    for t in &tables {
        let columns: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info(\"{t}\")"))
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .flatten()
            .collect();
        for name in std::iter::once(t).chain(columns.iter()) {
            if re.is_match(name) && !SAFE_COLUMNS.contains(&name.as_str()) {
                bad.push(format!("{}: {t}.{name}", db.display()));
            }
        }
    }
    bad
}

#[tokio::test]
async fn learned_data_and_hints_stay_anonymous_end_to_end() {
    let dir = std::env::temp_dir().join(format!("magnet-privacy-{}", rand::random::<u64>()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("registry.yaml"), REGISTRY).unwrap();

    let mut ndjson = vec![
        line(&format!("https://www.{SEEN}?utm=1#x"), 3, None),
        line(&format!("https://{BELOW_K}"), 2, None),
    ];
    ndjson.extend(identifying_lines());
    std::fs::write(dir.join("learn.ndjson"), ndjson.join("\n")).unwrap();
    let report: Value =
        serde_json::from_slice(&run(&dir, &["learn", "learn.ndjson"]).stdout).unwrap();
    assert_eq!(report["accepted"], 2, "{report}");
    assert_eq!(report["rejected"], 5, "{report}");

    let export = String::from_utf8(run(&dir, &["learn", "--export"]).stdout).unwrap();
    assert!(export.contains(SEEN), "{export}");
    assert!(!export.contains(BELOW_K) && !export.contains("someone"));
    let stats = String::from_utf8(run(&dir, &["learn", "--stats"]).stdout).unwrap();
    assert!(stats.contains("\"urls\": 1"), "{stats}");

    let (mut child, base, reader) = spawn_server(&dir);
    let c = reqwest::Client::new();
    let hinted = json!({
        "query": QUERY, "k": 5, "fetch": false, "lang": "en",
        "prefer_domains": [PREFER, "acme-widgets.com"], "avoid_domains": [AVOID],
    });
    for _ in 0..2 {
        let resp = c
            .post(format!("{base}/search"))
            .json(&hinted)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let body = resp.text().await.unwrap();
        assert!(body.contains(SEEN), "{body}");
        assert!(!body.contains(BELOW_K) && !body.contains(PREFER) && !body.contains(AVOID));
    }
    let too_many: Vec<String> = (0..21).map(|i| format!("d{i}.com")).collect();
    for bad in [
        json!({"query": QUERY, "prefer_domains": too_many}),
        json!({"query": QUERY, "lang": "english"}),
    ] {
        let r = c
            .post(format!("{base}/search"))
            .json(&bad)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 400);
    }
    let learn: Value = c
        .post(format!("{base}/learn"))
        .bearer_auth("adm")
        .body(identifying_lines().join("\n"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        (learn["accepted"].as_u64(), learn["rejected"].as_u64()),
        (Some(0), Some(5))
    );
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
    assert_eq!(stats["products"], 1);
    assert!(!stats.to_string().contains(BELOW_K));
    child.kill().unwrap();
    child.wait().unwrap();
    let mut logs = reader.join().unwrap().join("\n");

    let mut mcp = magnet(&dir)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let call = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "magnet_search", "arguments": {
            "query": QUERY, "fetch": false, "lang": "en",
            "prefer_domains": [PREFER], "avoid_domains": [AVOID],
        }},
    });
    mcp.stdin
        .take()
        .unwrap()
        .write_all(format!("{call}\n").as_bytes())
        .unwrap();
    let out = mcp.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(SEEN), "{stdout}");
    assert!(!stdout.contains(PREFER) && !stdout.contains(AVOID));
    logs.push_str(&String::from_utf8_lossy(&out.stderr));

    assert!(logs.contains("\"route\":\"/search\""), "{logs}");
    assert!(
        logs.contains(QUERY),
        "debug logs should carry the query: {logs}"
    );
    for hint in [
        PREFER,
        AVOID,
        "hintprefer",
        "hintavoid",
        "prefer_domains",
        "avoid_domains",
    ] {
        assert!(!logs.contains(hint), "{hint} leaked into logs: {logs}");
    }

    let files = files_under(&dir);
    for db in DATABASES {
        assert!(
            files
                .iter()
                .any(|f| f.file_name().is_some_and(|n| n == *db)),
            "{db} was not created"
        );
    }
    let mut bad = Vec::new();
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for needle in [PREFER, AVOID, "someone@example.com"] {
            if text.contains(needle) && !f.ends_with("learn.ndjson") {
                bad.push(format!("{needle} found in {}", f.display()));
            }
        }
        if f.extension().is_some_and(|e| e == "sqlite") {
            bad.extend(identifying_names(f));
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}
