//! Converts Adblock Plus / EasyList / uBlock network filters into WebKit
//! content-blocker JSON, which WebKit compiles to bytecode and runs natively.
//!
//! Supported: `||host^` and plain patterns with `*`, `^`, `|` anchors;
//! `@@` exceptions; options third-party/first-party, resource types, popup,
//! document/doc/all, domain=, match-case. Cosmetic (`##`) rules, regex rules
//! and options WebKit can't express (redirect=, csp=, removeparam, ...) are
//! skipped rather than approximated.

use serde_json::{Value as Json, json};

/// WebKit refuses lists above ~150k rules; stay well below.
const MAX_RULES: usize = 120_000;

/// Every resource type except top-level documents and popups.
const SUBRESOURCES: &[&str] =
    &["image", "style-sheet", "script", "font", "raw", "svg-document", "media", "ping", "websocket", "other"];

pub struct Converted {
    pub json: String,
    pub rules: usize,
    pub skipped: usize,
}

pub fn convert(text: &str) -> Converted {
    let mut blocks = Vec::new();
    let mut allows = Vec::new();
    let mut skipped = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('!') || line.starts_with('[') {
            continue;
        }
        if line.contains("##") || line.contains("#@#") || line.contains("#?#") || line.contains("#$#") || line.contains("#%#") {
            continue; // cosmetic
        }
        match convert_rule(line) {
            Some((rule, true)) => allows.push(rule),
            Some((rule, false)) => blocks.push(rule),
            None => skipped += 1,
        }
    }
    blocks.truncate(MAX_RULES.saturating_sub(allows.len()));
    let rules = blocks.len() + allows.len();
    // Exceptions only override rules that come before them.
    blocks.extend(allows);
    Converted { json: Json::Array(blocks).to_string(), rules, skipped }
}

/// Returns (rule, is_exception).
fn convert_rule(line: &str) -> Option<(Json, bool)> {
    let (exception, line) = match line.strip_prefix("@@") {
        Some(rest) => (true, rest),
        None => (false, line),
    };
    let (pattern, options) = split_options(line);
    if pattern.len() > 1 && pattern.starts_with('/') && pattern.ends_with('/') {
        return None; // regex rule
    }
    if !pattern.is_ascii() {
        return None;
    }

    let mut types: Vec<&str> = Vec::new();
    let mut load_type: Option<&str> = None;
    let mut if_domains: Vec<String> = Vec::new();
    let mut unless_domains: Vec<String> = Vec::new();
    let mut case_sensitive = false;
    let mut subframe_only = false;
    let mut all = false;

    for opt in options.split(',').filter(|o| !o.is_empty()) {
        match opt {
            "third-party" | "3p" => load_type = Some("third-party"),
            "~third-party" | "~3p" | "first-party" | "1p" => load_type = Some("first-party"),
            "match-case" => case_sensitive = true,
            "important" => {}
            "all" => all = true,
            "script" => types.push("script"),
            "image" => types.push("image"),
            "stylesheet" | "css" => types.push("style-sheet"),
            "font" => types.push("font"),
            "media" => types.push("media"),
            "xmlhttprequest" | "xhr" => types.push("raw"),
            "websocket" => types.push("websocket"),
            "ping" | "beacon" => types.push("ping"),
            "other" | "object" => types.push("other"),
            "popup" | "popunder" => types.push("popup"),
            "document" | "doc" if !exception => types.push("document"),
            "subdocument" | "frame" => subframe_only = true,
            _ => {
                {
                    let list = opt.strip_prefix("domain=")?;
                    for d in list.split('|') {
                        let (neg, d) = match d.strip_prefix('~') {
                            Some(rest) => (true, rest),
                            None => (false, d),
                        };
                        if d.is_empty() || !d.is_ascii() || d.contains('*') {
                            return None;
                        }
                        let d = format!("*{}", d.to_ascii_lowercase());
                        if neg { unless_domains.push(d) } else { if_domains.push(d) }
                    }
                }
            }
        }
    }

    let (url_filter, host_anchored) = pattern_to_regex(pattern)?;

    let mut trigger = json!({ "url-filter": url_filter });
    if case_sensitive {
        trigger["url-filter-is-case-sensitive"] = json!(true);
    }
    if subframe_only {
        if !types.is_empty() {
            return None; // can't combine frame-only with other types in one rule
        }
        types.push("document");
        trigger["load-context"] = json!(["child-frame"]);
    }
    if all {
        types.clear();
    } else if types.is_empty() && !host_anchored && !exception {
        // A typeless `||host^` block also stops top-level visits and redirects
        // to that host. Other typeless patterns stick to subresources so they
        // can't block the page you actually asked for.
        types.extend_from_slice(SUBRESOURCES);
    }
    if !types.is_empty() {
        types.sort_unstable();
        types.dedup();
        trigger["resource-type"] = json!(types);
    }
    if let Some(lt) = load_type {
        trigger["load-type"] = json!([lt]);
    }
    // WebKit allows if-domain or unless-domain, not both.
    if !if_domains.is_empty() {
        trigger["if-domain"] = json!(if_domains);
    } else if !unless_domains.is_empty() {
        trigger["unless-domain"] = json!(unless_domains);
    }
    let action = if exception { "ignore-previous-rules" } else { "block" };
    Some((json!({ "trigger": trigger, "action": { "type": action } }), exception))
}

/// Split `pattern$options`, only treating `$` as a separator when what
/// follows looks like an option list.
fn split_options(line: &str) -> (&str, &str) {
    if let Some(pos) = line.rfind('$') {
        let opts = &line[pos + 1..];
        let looks_like_options = !opts.is_empty()
            && opts.chars().all(|c| c.is_ascii_alphanumeric() || "~,=|._-*".contains(c));
        if looks_like_options {
            return (&line[..pos], opts);
        }
    }
    (line, "")
}

/// ABP pattern -> WebKit url-filter regex. Returns (regex, is `||host` rule).
fn pattern_to_regex(pattern: &str) -> Option<(String, bool)> {
    let mut out = String::new();
    let mut rest = pattern;
    let mut host_anchored = false;

    if let Some(r) = rest.strip_prefix("||") {
        out.push_str("^[^:]+://+([^:/]+\\.)?");
        rest = r;
        host_anchored = true;
        // `||host^` -> the host must end right there.
        let host_end = rest.find(['^', '/', '*', ':', '?']).unwrap_or(rest.len());
        let (host, tail) = rest.split_at(host_end);
        if host.is_empty() {
            return None;
        }
        out.push_str(&escape(&host.to_ascii_lowercase()));
        rest = match tail.strip_prefix('^') {
            Some(t) => {
                out.push_str("[/:]");
                t
            }
            None => tail,
        };
    } else if let Some(r) = rest.strip_prefix('|') {
        out.push('^');
        rest = r;
    }

    let (body, end_anchor) = match rest.strip_suffix('|') {
        Some(b) => (b, true),
        None => (rest, false),
    };
    if body.contains('|') {
        return None;
    }
    let chars: Vec<char> = body.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        match c {
            '*' => out.push_str(".*"),
            // A trailing `^` also matches end-of-URL; dropping it is the
            // closest WebKit-compatible approximation.
            '^' if i + 1 == chars.len() => {}
            '^' => out.push_str("[/:?&=]"),
            c => out.push_str(&escape(&c.to_string())),
        }
    }
    if end_anchor {
        out.push('$');
    }
    let trimmed = out.trim_start_matches(".*").trim_end_matches(".*");
    let out = if trimmed.is_empty() { ".*".to_string() } else { trimmed.to_string() };
    Some((out, host_anchored))
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.+?()[]{}$^|".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(rule: &str) -> Json {
        convert_rule(rule).map(|(r, _)| r).unwrap_or(Json::Null)
    }

    #[test]
    fn host_rules_block_everything_including_redirects() {
        let r = one("||ads.example.com^");
        assert_eq!(r["trigger"]["url-filter"], "^[^:]+://+([^:/]+\\.)?ads\\.example\\.com[/:]");
        assert!(r["trigger"].get("resource-type").is_none());
        assert_eq!(r["action"]["type"], "block");
    }

    #[test]
    fn options() {
        let r = one("||t.com^$third-party,script,domain=a.com|b.org");
        assert_eq!(r["trigger"]["load-type"], json!(["third-party"]));
        assert_eq!(r["trigger"]["resource-type"], json!(["script"]));
        assert_eq!(r["trigger"]["if-domain"], json!(["*a.com", "*b.org"]));
        assert_eq!(one("||p.com^$popup")["trigger"]["resource-type"], json!(["popup"]));
        assert_eq!(one("||f.com^$subdocument")["trigger"]["load-context"], json!(["child-frame"]));
        assert!(one("||x.com^$redirect=noop.js").is_null());
        assert!(one("/banner[0-9]+/").is_null());
    }

    #[test]
    fn generic_patterns_skip_top_level_documents() {
        let r = one("/ads/banner*.gif");
        assert_eq!(r["trigger"]["url-filter"], "/ads/banner.*\\.gif");
        let types = r["trigger"]["resource-type"].as_array().unwrap();
        assert!(!types.contains(&json!("document")));
    }

    #[test]
    fn exceptions_come_last() {
        let c = convert("@@||good.com^\n||bad.com^\nexample.com##.ad\n! comment");
        let rules: Json = serde_json::from_str(&c.json).unwrap();
        assert_eq!(c.rules, 2);
        assert_eq!(rules[0]["action"]["type"], "block");
        assert_eq!(rules[1]["action"]["type"], "ignore-previous-rules");
    }
}
