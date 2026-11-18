//! Addresses: what the user typed into one, the site one belongs to, and
//! links meant for other apps.

/// Turn what the user typed into an address: a URL as it is, a host name
/// over HTTPS (http:// for this computer and bare IPv4 addresses), and
/// anything else a search with `search_url` (`%s` is the query).
/// "ai <question>" asks Duck.ai instead.
pub fn resolve_input(text: &str, search_url: &str) -> String {
    let t = text.trim();
    if let Some(q) = t.strip_prefix("ai ").map(str::trim).filter(|q| !q.is_empty()) {
        return format!("https://duckduckgo.com/?q={}&ia=chat", escape(q));
    }
    let has_scheme =
        t.contains("://") || ["about:", "wisp:", "data:", "file:", "view-source:"].iter().any(|s| t.starts_with(s));
    if has_scheme {
        return t.to_string();
    }
    if !t.contains(char::is_whitespace) {
        let host = t.split(['/', ':', '?', '#']).next().unwrap_or("");
        if host == "localhost" || host.parse::<std::net::Ipv4Addr>().is_ok() {
            return format!("http://{t}");
        }
        if host.contains('.') && !host.starts_with('.') && !host.ends_with('.') {
            return format!("https://{t}");
        }
    }
    search_url.replace("%s", &escape(t))
}

/// Percent-encode all but letters, digits and `-._~`, as GLib's
/// `g_uri_escape_string(s, NULL, FALSE)` does.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A link for another app (`mailto:`, `magnet:`, `claude://`, ...): any
/// well-formed scheme that isn't one of the web's or Wisp's own.
pub fn is_app_link(uri: &str) -> bool {
    let Some((scheme, _)) = uri.split_once(':') else { return false };
    let web = [
        "http", "https", "file", "about", "data", "blob", "javascript", "wisp", "wisp-plugin", "view-source", "ws", "wss",
        // Gecko's own: extension pages (Wisp's plugins on Android) and built-in files.
        "moz-extension", "resource",
    ];
    let valid = !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
    valid && !web.contains(&scheme.to_ascii_lowercase().as_str())
}

/// The site an address belongs to: its last two labels (three for names
/// like example.co.uk), or its IP address.
pub fn site_of(uri: &str) -> String {
    let host = crate::privacy::scheme_and_host(uri).map(|(_, h)| h).unwrap_or("").to_ascii_lowercase();
    if host.parse::<std::net::IpAddr>().is_ok() {
        return host;
    }
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    let n = labels.len();
    if n <= 2 {
        return labels.join(".");
    }
    // Second-level public suffixes like co.uk, com.au, org.br.
    let take = if labels[n - 1].len() == 2 && ["co", "com", "org", "net", "gov", "ac", "edu"].contains(&labels[n - 2]) {
        3
    } else {
        2
    };
    labels[n - take..].join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_links() {
        for uri in ["claude://claude.ai/login?code=x", "mailto:a@b.c", "magnet:?xt=urn", "steam://run/1"] {
            assert!(is_app_link(uri), "{uri}");
        }
        for uri in ["https://claude.ai", "about:blank", "wisp:history", "wisp-plugin://x/p.html", "data:text/plain,x", "blob:https://a/b", "localhost", "moz-extension://0a1b/list.html"] {
            assert!(!is_app_link(uri), "{uri}");
        }
    }

    #[test]
    fn typed_addresses() {
        let s = "https://duckduckgo.com/?q=%s&k1=-1";
        assert_eq!(resolve_input("example.com", s), "https://example.com");
        assert_eq!(resolve_input(" localhost:8000/x ", s), "http://localhost:8000/x");
        assert_eq!(resolve_input("192.168.1.1", s), "http://192.168.1.1");
        assert_eq!(resolve_input("https://a.b/c", s), "https://a.b/c");
        assert_eq!(resolve_input("rust lang", s), "https://duckduckgo.com/?q=rust%20lang&k1=-1");
        assert_eq!(resolve_input("é&x", s), "https://duckduckgo.com/?q=%C3%A9%26x&k1=-1");
        assert_eq!(resolve_input("ai why?", s), "https://duckduckgo.com/?q=why%3F&ia=chat");
        assert_eq!(resolve_input(".hidden", s), "https://duckduckgo.com/?q=.hidden&k1=-1");
    }

    #[test]
    fn sites() {
        assert_eq!(site_of("https://a.b.example.com/x"), "example.com");
        assert_eq!(site_of("https://www.bbc.co.uk/"), "bbc.co.uk");
        assert_eq!(site_of("http://10.0.0.1:8080/"), "10.0.0.1");
    }
}
