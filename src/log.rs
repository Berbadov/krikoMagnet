use std::sync::OnceLock;

use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub enum Level {
    Off,
    Error,
    Info,
    Debug,
}

impl Level {
    pub fn parse(raw: &str) -> Level {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "0" => Level::Off,
            "error" | "warn" | "warning" => Level::Error,
            "debug" | "trace" => Level::Debug,
            _ => Level::Info,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Error => "error",
            Level::Info => "info",
            Level::Debug => "debug",
        }
    }
}

pub fn level() -> Level {
    static LEVEL: OnceLock<Level> = OnceLock::new();
    *LEVEL.get_or_init(|| {
        std::env::var("MAGNET_LOG")
            .or_else(|_| std::env::var("KRIKO_LOG"))
            .map(|v| Level::parse(&v))
            .unwrap_or(Level::Info)
    })
}

pub fn enabled(at: Level) -> bool {
    at != Level::Off && at <= level()
}

pub fn emit(at: Level, mut line: Value) {
    if !enabled(at) {
        return;
    }
    if let Some(obj) = line.as_object_mut() {
        obj.insert("ts".into(), json!(chrono::Utc::now().to_rfc3339()));
        obj.insert("level".into(), json!(at.name()));
    }
    eprintln!("{line}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_levels_and_orders_them() {
        assert_eq!(Level::parse("OFF"), Level::Off);
        assert_eq!(Level::parse("warn"), Level::Error);
        assert_eq!(Level::parse("INFO"), Level::Info);
        assert_eq!(Level::parse("debug"), Level::Debug);
        assert_eq!(Level::parse("bogus"), Level::Info);
        assert!(Level::Error < Level::Info && Level::Info < Level::Debug);
    }
}
