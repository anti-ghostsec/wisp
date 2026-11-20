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

/// What another app asked Wisp to search for (Android's web search
/// intents): a web address as `resolve_input` makes it, anything else
/// (`javascript:`, `file:`, `wisp:`, ...) searched for as text. None if the
/// search engine itself isn't a web address.
pub fn resolve_search(text: &str, search_url: &str) -> Option<String> {
    let is_web = |u: &str| {
        let scheme = u.split_once(':').map(|(s, _)| s.to_ascii_lowercase());
        matches!(scheme.as_deref(), Some("http" | "https"))
    };
    let resolved = resolve_input(text, search_url);
    if is_web(&resolved) {
        return Some(resolved);
    }
    let search = search_url.replace("%s", &escape(text.trim()));
    is_web(&search).then_some(search)
}

/// Whether copied text is safe to open at once with Paste and Go: a web
/// address, a host name with a known top-level domain, or words to search
/// for. Anything else (one word with digits or symbols, an odd host name)
/// may be a password or a key, which shouldn't go to a DNS server or a
/// search engine; the person checks it and presses Enter.
pub fn paste_can_go(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    if t.starts_with("https://") || t.starts_with("http://") {
        return !t.contains(char::is_whitespace);
    }
    if t.contains(char::is_whitespace) {
        // Words to search for, each one made of letters (and the odd
        // apostrophe, hyphen or ending mark), not a passphrase of symbols.
        return t.split_whitespace().count() >= 2
            && t.split_whitespace().all(|w| {
                let w = w.trim_end_matches(['?', '!', ',', '.', ':', ';']);
                w.chars().all(|c| c.is_alphanumeric() || "'-\"".contains(c))
                    && w.chars().any(char::is_alphabetic)
            });
    }
    if t.chars().all(char::is_alphabetic) {
        return true;
    }
    let host = t.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.split(':').next().unwrap_or("").to_ascii_lowercase();
    if host == "localhost" || host.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }
    let labels: Vec<&str> = host.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty() && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        && KNOWN_TLDS.contains(labels.last().unwrap_or(&""))
}

/// Common top-level domains, for telling a host name from a password with
/// a dot in it.
const KNOWN_TLDS: &[&str] = &[
    "com", "org", "net", "edu", "gov", "mil", "int", "info", "io", "dev", "app", "ai", "me", "co", "uk", "us", "ca",
    "au", "nz", "de", "fr", "nl", "be", "ch", "at", "it", "es", "pt", "se", "no", "dk", "fi", "is", "ie", "pl", "cz",
    "sk", "hu", "ro", "bg", "gr", "tr", "ru", "ua", "by", "ee", "lv", "lt", "jp", "cn", "kr", "tw", "hk", "sg", "in",
    "id", "th", "vn", "my", "ph", "br", "ar", "mx", "cl", "pe", "za", "eu", "tv", "fm", "gg", "xyz", "online", "site",
    "blog", "news", "page", "wiki", "social", "onion", "tech", "store", "shop", "cloud", "one", "ly",
    "sh", "so", "to", "cc", "ws", "su", "il", "ir", "pk", "ng", "ke", "eg", "sa", "ae", "lu", "si", "hr", "rs", "local",
];

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
        "http", "https", "file", "about", "data", "blob", "javascript", "wisp", "wisp-plugin", "wisp-allow-http", "view-source",
        "ws", "wss",
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
    fn searches_from_other_apps_stay_on_the_web() {
        let s = "https://duckduckgo.com/?q=%s";
        assert_eq!(resolve_search("example.com", s).unwrap(), "https://example.com");
        assert_eq!(resolve_search("javascript:alert(1)", s).unwrap(), "https://duckduckgo.com/?q=javascript%3Aalert%281%29");
        assert_eq!(resolve_search("file:///etc/passwd", s).unwrap(), "https://duckduckgo.com/?q=file%3A%2F%2F%2Fetc%2Fpasswd");
        assert!(resolve_search("wisp:history", s).unwrap().starts_with("https://duckduckgo.com/"));
        assert!(resolve_search("rust", "file:///x?%s").is_none());
    }

    #[test]
    fn pastes_that_can_go() {
        for t in ["https://example.com/a?b=c", "example.com", "news.bbc.co.uk/x", "localhost:8000", "rust lang", "weather", "what's new?"] {
            assert!(paste_can_go(t), "{t}");
        }
        // Passwords and keys stay in the address bar until Enter.
        for t in ["Xk9.mQ2v!pz", "hunter2", "a8f.k2l", "tR4c.e9Lq.zz", "sk-abc123.def", "p@ss w0rd!#", "", "ghp_x1Y2z3"] {
            assert!(!paste_can_go(t), "{t}");
        }
    }

    #[test]
    fn sites() {
        assert_eq!(site_of("https://a.b.example.com/x"), "example.com");
        assert_eq!(site_of("https://www.bbc.co.uk/"), "bbc.co.uk");
        assert_eq!(site_of("http://10.0.0.1:8080/"), "10.0.0.1");
    }
}
