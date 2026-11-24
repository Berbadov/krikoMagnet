use dom_smoothie::{Config, Readability, TextMode};
use scraper::{Html, Selector};

pub const SOFT_MIN_CHARS: usize = 300;

const CHALLENGE_MARKERS: &[&str] = &[
    "just a moment",
    "enable javascript and cookies",
    "verify you are human",
    "captcha",
    "attention required",
    "access denied",
    "checking your browser",
];

pub fn extract_text(html: &str, url: Option<&str>, min_chars: usize) -> String {
    if html.trim().is_empty() {
        return String::new();
    }
    let cfg = Config {
        text_mode: TextMode::Markdown,
        ..Default::default()
    };
    let mut best = String::new();
    if let Ok(mut r) = Readability::new(html, url, Some(cfg))
        && let Ok(article) = r.parse()
    {
        let md = article.text_content.trim();
        if md.chars().count() >= min_chars {
            return md.to_string();
        }
        best = md.to_string();
        if let Ok(conv) = htmd::convert(&*article.content) {
            let trimmed = conv.trim();
            if trimmed.chars().count() >= min_chars {
                return trimmed.to_string();
            }
            if trimmed.len() > best.len() {
                best = trimmed.to_string();
            }
        }
    }

    // Fallback: visible text from body/main via HTML5 parser, skipping script/style.
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
                        Some("script" | "style" | "noscript" | "svg")
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
    if fallback.len() > best.len() {
        fallback
    } else {
        best
    }
}

pub fn classify_fetch(status: Option<u16>, status_tag: &str, clean_chars: usize, text: &str) -> &'static str {
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
    let marker = CHALLENGE_MARKERS.iter().any(|m| low.contains(m));
    if clean_chars < SOFT_MIN_CHARS || marker {
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
        assert_eq!(classify_fetch(Some(200), "200", 50, "Just a moment..."), "soft-block");
    }
}
