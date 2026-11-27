use std::collections::HashSet;
use std::sync::LazyLock;

use dom_smoothie::{Config, Readability, TextMode};
use regex::Regex;
use scraper::{Html, Selector};
use serde_json::Value;

pub const SOFT_MIN_CHARS: usize = 300;

const STRONG_CHALLENGE_MARKERS: &[&str] = &[
    "just a moment",
    "enable javascript and cookies",
    "verify you are human",
    "checking your browser",
    "a required part of this site",
];

const SOFT_CHALLENGE_MARKERS: &[&str] = &[
    "enable javascript",
    "please check your cookies",
    "browser settings",
    "captcha",
    "attention required",
    "access denied",
];

const PROSE_JSON_KEYS: &[&str] = &[
    "articleBody",
    "reviewBody",
    "blurb",
    "summary",
    "verdict",
    "description",
    "body",
    "text",
    "content",
    "abstract",
    "prose",
];

static HTML_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<[^>]{1,200}>").expect("valid html tag regex"));
static JSON_PROSE_KV_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#""([a-zA-Z0-9_]*(?:blurb|summary|verdict|description|articleBody|reviewBody|body|content|text|abstract|prose))"\s*:\s*"((?:\\.|[^"\\])+)""#,
    )
    .expect("valid json prose regex")
});

fn sentence_count(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut n = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        if matches!(b, b'.' | b'!' | b'?') {
            let next = bytes.get(i + 1).copied();
            if next.is_none_or(|c| c.is_ascii_whitespace()) && i > 8 {
                n += 1;
            }
        }
    }
    n
}

fn strip_inline_html(s: &str) -> String {
    let no_tags = if s.contains('<') && s.contains('>') {
        HTML_TAG_RE.replace_all(s, " ")
    } else {
        std::borrow::Cow::Borrowed(s)
    };
    no_tags.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn accept_prose_snippet(s: &str) -> Option<String> {
    let cleaned = strip_inline_html(s);
    let chars = cleaned.chars().count();
    if chars < 45 {
        return None;
    }
    let words = cleaned.split_whitespace().count();
    if words < 7 {
        return None;
    }
    if cleaned.starts_with('{') || cleaned.starts_with('[') || cleaned.contains("function(") {
        return None;
    }
    let low = cleaned.to_lowercase();
    if low.starts_with("http://") || low.starts_with("https://") {
        return None;
    }
    // Must contain sentence punctuation or substantial multi-word length
    if sentence_count(&cleaned) == 0 && words < 14 {
        return None;
    }
    let alnum_or_space = cleaned
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace() || matches!(c, '.' | ',' | ':' | ';'))
        .count();
    if (alnum_or_space as f64) / (chars as f64) < 0.72 {
        return None;
    }
    Some(cleaned)
}

fn push_unique_prose(cleaned: String, seen: &mut HashSet<String>, out: &mut Vec<String>) {
    let key = cleaned.to_lowercase();
    if !seen.insert(key.clone()) {
        return;
    }
    if out.iter().any(|existing| {
        let ex_low = existing.to_lowercase();
        ex_low.contains(&key) || key.contains(&ex_low)
    }) {
        return;
    }
    // Check 4-word overlap so overlapping JSON summary/blurb/meta strings don't inflate trigram_repeat.
    let words: Vec<&str> = key.split_whitespace().collect();
    if words.len() >= 8 {
        let cur_grams: HashSet<(&str, &str, &str, &str)> =
            words.windows(4).map(|w| (w[0], w[1], w[2], w[3])).collect();
        if !cur_grams.is_empty() {
            for prev in out.iter() {
                let prev_low = prev.to_lowercase();
                let p_words: Vec<&str> = prev_low.split_whitespace().collect();
                if p_words.len() >= 8 {
                    let overlap = p_words
                        .windows(4)
                        .filter(|w| cur_grams.contains(&(w[0], w[1], w[2], w[3])))
                        .count();
                    if overlap * 2 >= cur_grams.len() {
                        return;
                    }
                }
            }
        }
    }
    out.push(cleaned);
}

fn collect_json_value_prose(val: &Value, seen: &mut HashSet<String>, out: &mut Vec<String>) {
    match val {
        Value::Object(map) => {
            for (k, v) in map {
                let k_low = k.to_lowercase();
                if PROSE_JSON_KEYS
                    .iter()
                    .any(|pk| k_low == pk.to_lowercase() || k_low.ends_with(&pk.to_lowercase()))
                    && let Some(s) = v.as_str()
                    && let Some(cleaned) = accept_prose_snippet(s)
                {
                    push_unique_prose(cleaned, seen, out);
                }
                collect_json_value_prose(v, seen, out);
            }
        }
        Value::Array(arr) => {
            for item in arr {
                collect_json_value_prose(item, seen, out);
            }
        }
        _ => {}
    }
}

fn extract_from_json_blob(blob: &str, seen: &mut HashSet<String>, out: &mut Vec<String>) {
    if let Ok(val) = serde_json::from_str::<Value>(blob.trim()) {
        collect_json_value_prose(&val, seen, out);
        return;
    }
    for cap in JSON_PROSE_KV_RE.captures_iter(blob) {
        if let Some(raw_val) = cap.get(2) {
            if raw_val.as_str().len() < 45 || raw_val.as_str().len() > 6000 {
                continue;
            }
            let quoted = format!("\"{}\"", raw_val.as_str());
            let decoded = serde_json::from_str::<String>(&quoted)
                .unwrap_or_else(|_| raw_val.as_str().to_string());
            if let Some(cleaned) = accept_prose_snippet(&decoded) {
                push_unique_prose(cleaned, seen, out);
            }
        }
    }
}

/// Rescues real prose from JSON-LD and SSR hydrated component props (e.g., Next.js/Nuxt/Vue attrs).
fn extract_structured_prose(doc: &Html) -> String {
    let mut seen = HashSet::new();
    let mut chunks = Vec::new();

    if let Ok(script_sel) = Selector::parse(
        r#"script[type="application/ld+json"], script#__NEXT_DATA__, script[type="application/json"]"#,
    ) {
        for el in doc.select(&script_sel) {
            let text: String = el.text().collect();
            if text.len() > 60 {
                extract_from_json_blob(&text, &mut seen, &mut chunks);
            }
        }
    }

    // Hydrated SSR component props in HTML attributes (scraper already decodes &quot; -> ").
    if let Ok(all_sel) = Selector::parse("*") {
        for el in doc.select(&all_sel) {
            for (_name, attr_val) in el.value().attrs() {
                if attr_val.len() >= 80 && attr_val.contains('{') && attr_val.contains(':') {
                    extract_from_json_blob(attr_val, &mut seen, &mut chunks);
                }
            }
        }
    }

    // Fallback scan across inline scripts only if JSON-LD + attributes produced little prose.
    if chunks.iter().map(|s| s.len()).sum::<usize>() < SOFT_MIN_CHARS
        && let Ok(script_sel) = Selector::parse("script:not([src])")
    {
        for el in doc.select(&script_sel) {
            let text: String = el.text().collect();
            if text.len() >= 120 && text.len() <= 400_000 {
                extract_from_json_blob(&text, &mut seen, &mut chunks);
            }
        }
    }

    chunks.join("\n\n")
}

pub fn extract_text(html: &str, url: Option<&str>, min_chars: usize) -> String {
    if html.trim().is_empty() {
        return String::new();
    }
    let cfg = Config {
        text_mode: TextMode::Markdown,
        ..Default::default()
    };
    let mut best = String::new();
    let fast_threshold = min_chars.max(1200);
    if let Ok(mut r) = Readability::new(html, url, Some(cfg))
        && let Ok(article) = r.parse()
    {
        let md = article.text_content.trim();
        let md_low = md.to_ascii_lowercase();
        if md.chars().count() >= fast_threshold
            && sentence_count(md) >= 4
            && !md_low.contains("browser is not supported")
        {
            return md.to_string();
        }
        best = md.to_string();
        if let Ok(conv) = htmd::convert(&*article.content) {
            let trimmed = conv.trim();
            let trim_low = trimmed.to_ascii_lowercase();
            if trimmed.chars().count() >= fast_threshold
                && sentence_count(trimmed) >= 4
                && !trim_low.contains("browser is not supported")
            {
                return trimmed.to_string();
            }
            if trimmed.len() > best.len() {
                best = trimmed.to_string();
            }
        }
    }

    // Fallback: visible text from main/article/body via HTML5 parser, skipping chrome & scripts.
    let doc = Html::parse_document(html);
    let body_sel = Selector::parse("main, article, body").ok();
    let mut fallback = String::new();
    if let Some(sel) = body_sel
        && let Some(root) = doc.select(&sel).next()
    {
        for node in root.descendants() {
            if let Some(txt) = node.value().as_text() {
                let inside_skip = node.ancestors().any(|a| {
                    matches!(
                        a.value().as_element().map(|e| e.name()),
                        Some(
                            "script"
                                | "style"
                                | "noscript"
                                | "svg"
                                | "nav"
                                | "footer"
                                | "header"
                                | "aside"
                        )
                    )
                });
                if !inside_skip {
                    let s = txt.trim();
                    if !s.is_empty() {
                        if !fallback.is_empty() {
                            fallback.push(' ');
                        }
                        fallback.push_str(s);
                    }
                }
            }
        }
    }
    let fallback = fallback.split_whitespace().collect::<Vec<_>>().join(" ");
    if sentence_count(&fallback) > sentence_count(&best)
        || (sentence_count(&fallback) == sentence_count(&best) && fallback.len() > best.len())
    {
        best = fallback;
    }

    let best_low = best.to_ascii_lowercase();
    let best_is_stub = best_low.contains("browser is not supported");
    if best.chars().count() < fast_threshold || sentence_count(&best) < 4 || best_is_stub {
        let structured = extract_structured_prose(&doc);
        if !structured.is_empty()
            && (best_is_stub
                || sentence_count(&structured) > sentence_count(&best)
                || structured.chars().count() > best.chars().count())
        {
            return structured;
        }
    }
    best
}

pub fn classify_fetch(
    status: Option<u16>,
    status_tag: &str,
    clean_chars: usize,
    text: &str,
) -> &'static str {
    if status_tag == "robots-denied" || status_tag == "robots" {
        return "robots-denied";
    }
    if let Some(code) = status {
        if matches!(code, 401 | 403 | 429) {
            return "hard-block";
        }
        if code != 200 {
            return "http-error";
        }
    } else {
        return "http-error";
    }
    let low = text.to_lowercase();
    let strong_marker = STRONG_CHALLENGE_MARKERS.iter().any(|m| low.contains(m));
    let soft_marker = clean_chars < 1500 && SOFT_CHALLENGE_MARKERS.iter().any(|m| low.contains(m));
    if clean_chars < SOFT_MIN_CHARS || strong_marker || soft_marker {
        "soft-block"
    } else {
        "success"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_article_markdown_without_scripts() {
        let html = r#"<html><head><title>Test</title></head><body>
            <nav>Home | About</nav>
            <script>var secret = 1;</script>
            <article><h1>LG C1 Burn-In Test</h1>
            <p>After 5,000 hours of CNN and gaming workloads, the LG C1 OLED showed minimal uniform wear across red and green subpixels, confirming strong compensation cycles during standby.</p>
            <p>We measured peak luminance, grayscale uniformity, and pixel-refresh behavior over 18 months of daily usage in our lab environment to verify long term panel durability.</p>
            </article></body></html>"#;
        let out = extract_text(html, Some("https://example.com/lg-c1"), 100);
        assert!(out.contains("LG C1"));
        assert!(!out.contains("var secret"));
        assert_eq!(classify_fetch(Some(200), "200", out.len(), &out), "success");
        assert_eq!(classify_fetch(Some(403), "403", 0, ""), "hard-block");
        assert_eq!(
            classify_fetch(Some(200), "200", 50, "Just a moment..."),
            "soft-block"
        );
    }

    #[test]
    fn rescues_ssr_hydrated_props_and_ignores_nav_chrome() {
        let html = r#"<html><head><title>LG C1 OLED Review</title>
            <script type="application/ld+json">{"@type":"Review","reviewBody":"The LG C1 OLED delivers exceptional dark-room contrast and low input lag for console and PC gaming workloads."}</script>
            </head><body>
            <header><nav>TVs Headphones Monitors Soundbars Laptops Deals About Us Reviews Best TVs 4k Gaming Cinema Sports</nav></header>
            <div data-review-props="{&quot;blurb&quot;:&quot;The LG C1 has a low risk of permanent burn-in under varied daily viewing, though static news tickers at maximum OLED light increase red subpixel wear over thousands of hours.&quot;,&quot;verdict&quot;:&quot;Overall picture quality and HDMI 2.1 gaming responsiveness make the LG C1 an outstanding reference display for mixed home theater setups.&quot;}"></div>
            <footer>Privacy Policy Terms Contact Us</footer>
            </body></html>"#;
        let out = extract_text(
            html,
            Some("https://www.rtings.com/tv/reviews/lg/c1-oled"),
            SOFT_MIN_CHARS,
        );
        assert!(
            !out.contains("TVs Headphones Monitors"),
            "nav chrome leaked: {out}"
        );
        assert!(
            out.contains("permanent burn-in"),
            "missing SSR blurb: {out}"
        );
        assert!(
            out.contains("dark-room contrast"),
            "missing JSON-LD reviewBody: {out}"
        );
        assert_eq!(classify_fetch(Some(200), "200", out.len(), &out), "success");
    }

    #[test]
    fn classifies_js_wall_as_soft_block() {
        let wall = "A required part of this site couldn't load. This may be due to a browser extension, network issues, or browser settings. Please enable javascript to view this article.";
        assert_eq!(
            classify_fetch(Some(200), "200", wall.len(), wall),
            "soft-block"
        );
        // Long real article mentioning "browser settings" should remain success.
        let long_doc = format!(
            "{} Understanding browser settings helps users inspect developer tools, performance timelines, and cache headers during routine debugging sessions.",
            "Modern web engineering covers responsive layouts, semantic HTML, accessible keyboard navigation, and server rendering optimizations. ".repeat(15)
        );
        assert_eq!(
            classify_fetch(Some(200), "200", long_doc.len(), &long_doc),
            "success"
        );
    }

    #[test]
    fn json_prose_fallback_regex_compiles_and_extracts() {
        let html = r#"<html><body>
            <script>window.__BOOT__ = { config: fn(), "blurb":"The OLED panel passed our 5,000-hour burn-in protocol with slight red subpixel fade near static logos during bright daytime testing cycles." };</script>
            </body></html>"#;
        let out = extract_text(html, Some("https://example.com/test"), SOFT_MIN_CHARS);
        assert!(out.contains("5,000-hour burn-in protocol"), "got: {out}");
    }
}
