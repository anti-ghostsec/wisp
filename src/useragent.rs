//! Which browser Wisp tells sites it is (☰ → Identify as).
//!
//! `user_agent` in config.json holds a preset as `preset:<id>`, or any
//! other text as a custom user agent. Unset (null) means the default preset.
//!
//! The presets' version numbers follow the calendar (Chrome about
//! monthly, Safari each September), so they stay as current as the browsers
//! they name and don't stand out as old.

use std::time::{SystemTime, UNIX_EPOCH};

pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub tooltip: &'static str,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "safari-mac",
        label: "Safari on a Mac",
        tooltip: "Wisp's engine is Safari's (WebKit), so sites send code made for it. Common, and modern sites get their fast pages.",
    },
    Preset {
        id: "chrome-windows",
        label: "Chrome on Windows",
        tooltip: "The most common browser. Some sites then send Chrome-only code that WebKit runs badly or not at all, and a site that checks more than this text can tell it isn't Chrome.",
    },
    Preset {
        id: "webkit",
        label: "WebKit's own (Linux, Safari 60.5)",
        tooltip: "What WebKitGTK says by itself. Rare, so easy to pick out, and some sites (YouTube) treat it as an old Safari.",
    },
];

/// The preset used while `user_agent` is unset.
pub const DEFAULT: &str = "safari-mac";

/// Days since 1970-01-01, and today's (year, month).
fn today() -> (i64, i64, i64) {
    let days = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64 / 86_400).unwrap_or(20_000);
    // Civil date from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (days, year, month)
}

/// Chrome's current major version: 142 came out on 2025-10-28, and a new
/// one about every 32 days (four weeks, with a pause each December).
fn chrome_version(days: i64) -> i64 {
    let since = days - 20_389; // 2025-10-28
    142 + since.max(0) / 32
}

/// Safari's current major version: 26 from September 2025 (Apple numbers it
/// by the coming year), one more each September.
fn safari_version(year: i64, month: i64) -> i64 {
    let v = year - 2000 + i64::from(month >= 9);
    v.max(26)
}

/// The preset's id, if `setting` names one (or is unset: the default).
pub fn preset_of(setting: Option<&str>) -> Option<&'static str> {
    match setting {
        None | Some("") => Some(DEFAULT),
        Some(s) => s.strip_prefix("preset:").and_then(|id| PRESETS.iter().find(|p| p.id == id)).map(|p| p.id),
    }
}

/// What to give WebKit: None keeps WebKit's own.
pub fn resolve(setting: Option<&str>) -> Option<String> {
    let (days, year, month) = today();
    match preset_of(setting) {
        Some("safari-mac") => Some(format!(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/{}.0 Safari/605.1.15",
            safari_version(year, month)
        )),
        Some("chrome-windows") => Some(format!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{}.0.0.0 Safari/537.36",
            chrome_version(days)
        )),
        Some(_) => None,
        // Custom text; a "preset:" that isn't one is ignored.
        None => setting.filter(|s| !s.starts_with("preset:")).map(str::to_string),
    }
}

/// For a preset, a script for the page's own world that makes the other
/// things pages read about the browser agree with the user agent:
/// navigator.platform and navigator.vendor.
pub fn page_script(setting: Option<&str>) -> Option<String> {
    let (platform, vendor) = match preset_of(setting)? {
        "safari-mac" => ("MacIntel", "Apple Computer, Inc."),
        "chrome-windows" => ("Win32", "Google Inc."),
        _ => return None,
    };
    Some(format!(
        r#"(() => {{
  const define = (prop, value) => {{
    try {{ Object.defineProperty(Navigator.prototype, prop, {{ get: () => value, configurable: true }}); }} catch (_) {{}}
  }};
  define("platform", {platform:?});
  define("vendor", {vendor:?});
}})();"#
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_follow_the_calendar() {
        assert_eq!(chrome_version(20_389), 142);
        assert_eq!(chrome_version(20_389 + 32 * 10), 152);
        assert_eq!(safari_version(2025, 8), 26);
        assert_eq!(safari_version(2025, 9), 26);
        assert_eq!(safari_version(2026, 9), 27);
        let (_, year, month) = today();
        assert!(year >= 2025 && (1..=12).contains(&month));
    }

    #[test]
    fn settings_map_to_user_agents() {
        assert!(resolve(None).unwrap().contains("Macintosh"));
        assert!(resolve(Some("preset:chrome-windows")).unwrap().contains("Windows NT 10.0"));
        assert_eq!(resolve(Some("preset:webkit")), None);
        assert_eq!(resolve(Some("preset:nonsense")), None);
        assert_eq!(resolve(Some("MyBrowser/1.0")).as_deref(), Some("MyBrowser/1.0"));
        assert!(page_script(Some("MyBrowser/1.0")).is_none());
        assert!(page_script(None).unwrap().contains("MacIntel"));
    }
}
