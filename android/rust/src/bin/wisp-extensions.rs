//! Turns Wisp's plugins into built-in WebExtensions for GeckoView, one per
//! plugin, when the Android app is built:
//!
//!   wisp-extensions PLUGINS OUT [--list ID=FILE]...
//!
//! Each `OUT/<id>/` gets the plugin's files, a manifest.json (Manifest V3),
//! `wisp-api.js` (the `wisp` API, answered by the app through native
//! messaging) ahead of the plugin's isolated scripts, and its blocklists as
//! declarativeNetRequest rules: the plugin's own WebKit rules, and each
//! `--list` file (an EasyList-style list downloaded for plugin ID).
//!
//! Plugins that need a helper program (`native`) or a background page can't
//! run on Android and are left out.

use serde_json::{Value as Json, json};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use wisp_core::adblock;
use wisp_core::config::Config;
use wisp_core::plugins::{self, Plugin, RunAt, World};

/// Gecko's limit on one extension's static rules (GUARANTEED_MINIMUM_STATIC_RULES).
const RULES_PER_FILE: usize = 30_000;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(src), Some(out)) = (args.first(), args.get(1)) else {
        eprintln!("usage: wisp-extensions PLUGINS OUT [--list ID=FILE]...");
        std::process::exit(2);
    };
    let mut lists: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let mut rest = args[2..].iter();
    while let Some(a) = rest.next() {
        let spec = if a == "--list" { rest.next().cloned().unwrap_or_default() } else { a.strip_prefix("--list=").unwrap_or("").to_string() };
        match spec.split_once('=') {
            Some((id, file)) => lists.entry(id.to_string()).or_default().push(PathBuf::from(file)),
            None => {
                eprintln!("wisp-extensions: bad argument {a}");
                std::process::exit(2);
            }
        }
    }
    let out = Path::new(out);
    let _ = fs::remove_dir_all(out);
    fs::create_dir_all(out).expect("cannot create the output folder");

    plugins::set_built_in_dir(PathBuf::from(src));
    // Only the plugins in PLUGINS, none of this computer's own.
    let none = PathBuf::from("/nonexistent/wisp");
    wisp_core::config::set_dirs(wisp_core::config::Dirs { config: none.clone(), data: none.clone(), cache: none });
    let (found, _) = plugins::discover(&Config::default());
    let mut made = 0;
    for p in found.iter().filter(|p| p.dir.starts_with(src)) {
        let m = &p.manifest;
        if m.permissions.iter().any(|x| x == "native") || p.has_background() {
            println!("wisp-extensions: {} needs a helper program or background page; left out", m.id);
            continue;
        }
        let dir = out.join(&m.id);
        copy_dir(&p.dir, &dir).expect("cannot copy the plugin");
        let rules = build_rules(p, lists.get(&m.id).map(Vec::as_slice).unwrap_or_default());
        // Gecko takes up to 30,000 static rules from one extension: the
        // first lot go with the plugin, the rest into extensions of their
        // own ("<id>+2", ...), which the app turns on and off with it.
        let mut chunks = rules.chunks(RULES_PER_FILE);
        let mut resources = Vec::new();
        if let Some(first) = chunks.next() {
            fs::write(dir.join("dnr.json"), Json::from(first.to_vec()).to_string()).expect("cannot write rules");
            resources.push(json!({ "id": "rules", "enabled": true, "path": "dnr.json" }));
        }
        for (i, chunk) in chunks.enumerate() {
            let part = out.join(format!("{}+{}", m.id, i + 2));
            fs::create_dir_all(&part).expect("cannot create a rules extension");
            fs::write(part.join("dnr.json"), Json::from(chunk.to_vec()).to_string()).expect("cannot write rules");
            let manifest = json!({
                "manifest_version": 3,
                "name": format!("{} (rules {})", m.name, i + 2),
                "version": "1.0",
                "browser_specific_settings": { "gecko": { "id": format!("{}-{}@plugins.wisp", m.id, i + 2) } },
                "permissions": ["declarativeNetRequest"],
                "declarative_net_request": { "rule_resources": [{ "id": "rules", "enabled": true, "path": "dnr.json" }] },
            });
            fs::write(part.join("manifest.json"), serde_json::to_string_pretty(&manifest).unwrap()).expect("cannot write manifest.json");
        }
        fs::write(dir.join("wisp-api.js"), api_shim(p)).expect("cannot write the API");
        add_page_scripts(p, &dir);
        let manifest = manifest(p, &resources);
        fs::write(dir.join("manifest.json"), serde_json::to_string_pretty(&manifest).unwrap()).expect("cannot write manifest.json");
        println!("wisp-extensions: {} ({} blocking rules)", m.id, rules.len());
        made += 1;
    }
    println!("wisp-extensions: {made} extensions in {}", out.display());
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "__pycache__" {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            copy_dir(&path, &to.join(&name))?;
        } else {
            fs::copy(&path, to.join(&name))?;
        }
    }
    Ok(())
}

fn build_rules(p: &Plugin, lists: &[PathBuf]) -> Vec<Json> {
    let mut rules = Vec::new();
    for rel in &p.manifest.blocklists {
        let Some(text) = p.read(rel) else { continue };
        let Ok(webkit) = serde_json::from_str::<Json>(&text) else {
            eprintln!("wisp-extensions: {}: {rel} isn't JSON", p.manifest.id);
            continue;
        };
        rules.extend(adblock::webkit_to_dnr(&webkit, rules.len() as u64 + 1));
    }
    for file in lists {
        let text = fs::read_to_string(file).unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display()));
        let c = adblock::convert_dnr(&text, rules.len() as u64 + 1);
        println!("wisp-extensions: {}: {} rules, {} left out", file.display(), c.rules.len(), c.skipped);
        rules.extend(c.rules);
    }
    rules
}

/// The id GeckoView knows plugin `id` by.
fn extension_id(id: &str) -> String {
    format!("{id}@plugins.wisp")
}

fn is_page_script(matches: &[String], id: &str) -> bool {
    let own = format!("{}://{id}/", plugins::PAGE_SCHEME);
    !matches.is_empty() && matches.iter().all(|m| m.starts_with(&own))
}

fn manifest(p: &Plugin, rule_resources: &[Json]) -> Json {
    let m = &p.manifest;
    let mut scripts = Vec::new();
    for cs in &m.content_scripts {
        if is_page_script(&cs.matches, &m.id) {
            continue; // added to the plugin's pages instead
        }
        let mut entry = json!({
            "matches": cs.matches,
            "run_at": if cs.run_at == RunAt::DocumentStart { "document_start" } else { "document_end" },
            "all_frames": cs.all_frames,
        });
        if !cs.exclude_matches.is_empty() {
            entry["exclude_matches"] = json!(cs.exclude_matches);
        }
        if !cs.css.is_empty() {
            entry["css"] = json!(cs.css);
        }
        if !cs.js.is_empty() {
            if cs.world == World::Page {
                entry["world"] = json!("MAIN");
                entry["js"] = json!(cs.js);
            } else {
                let mut js = vec!["wisp-api.js".to_string()];
                js.extend(cs.js.iter().cloned());
                entry["js"] = json!(js);
            }
        }
        scripts.push(entry);
    }
    let mut permissions = vec!["nativeMessaging", "nativeMessagingFromContent", "geckoViewAddons"];
    if !rule_resources.is_empty() {
        permissions.push("declarativeNetRequest");
    }
    let version: String = m.version.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
    let mut manifest = json!({
        "manifest_version": 3,
        "name": m.name,
        "description": m.description,
        "version": if version.is_empty() { "1.0".to_string() } else { version },
        "browser_specific_settings": { "gecko": { "id": extension_id(&m.id) } },
        "permissions": permissions,
        "host_permissions": ["<all_urls>"],
        "content_scripts": scripts,
    });
    if !rule_resources.is_empty() {
        manifest["declarative_net_request"] = json!({ "rule_resources": rule_resources });
    }
    manifest
}

/// The plugin's scripts for its own pages (`wisp-plugin://<id>/...` on the
/// desktop) go into those pages, after the API.
fn add_page_scripts(p: &Plugin, dir: &Path) {
    let m = &p.manifest;
    let own = format!("{}://{}/", plugins::PAGE_SCHEME, m.id);
    let mut pages: Vec<String> = m.actions.iter().filter_map(|a| a.popup.clone().or(a.page.clone())).collect();
    pages.sort();
    pages.dedup();
    for page in pages {
        let path = dir.join(&page);
        let Ok(html) = fs::read_to_string(&path) else { continue };
        let depth = page.matches('/').count();
        let up = "../".repeat(depth);
        let mut tags = format!("<script src=\"{up}wisp-api.js\"></script>\n");
        for cs in m.content_scripts.iter().filter(|cs| is_page_script(&cs.matches, &m.id)) {
            let applies = cs.matches.iter().any(|pat| {
                let rel = pat.strip_prefix(&own).unwrap_or("");
                rel == "*" || rel == page || rel.strip_suffix('*').is_some_and(|pre| page.starts_with(pre))
            });
            if !applies {
                continue;
            }
            for css in &cs.css {
                tags.push_str(&format!("<link rel=\"stylesheet\" href=\"{up}{css}\">\n"));
            }
            for js in &cs.js {
                tags.push_str(&format!("<script src=\"{up}{js}\"></script>\n"));
            }
        }
        let html = match html.rfind("</body>") {
            Some(i) => format!("{}{tags}{}", &html[..i], &html[i..]),
            None => format!("{html}\n{tags}"),
        };
        fs::write(&path, html).expect("cannot write a plugin page");
    }
}

/// The `wisp` API of desktop Wisp's prelude, over GeckoView's native
/// messaging: calls go to the app ("wisp"), which answers them with
/// wisp-core, and events come back on a port.
fn api_shim(p: &Plugin) -> String {
    let meta = json!({ "id": p.manifest.id, "name": p.manifest.name, "version": p.manifest.version });
    format!(
        r#"(() => {{
  if (globalThis.wisp) return;
  const call = async (op, args = {{}}) => {{
    const text = await browser.runtime.sendNativeMessage("wisp", {{ op, args }});
    const reply = typeof text === "string" ? JSON.parse(text) : text;
    if (reply && reply.error) throw new Error(reply.error);
    return reply && reply.value !== undefined ? reply.value : null;
  }};
  const listeners = {{}};
  const handlers = [];
  const emit = (type, data) => {{
    for (const fn of listeners[type] || []) {{
      try {{ fn(data); }} catch (e) {{ console.error(e); }}
    }}
  }};
  try {{
    const port = browser.runtime.connectNative("wisp");
    port.onMessage.addListener((m) => m && m.type && emit(m.type, m.data));
  }} catch (e) {{ console.error("wisp: no event port", e); }}
  const store = (prefix) => Object.freeze({{
    get: (key) => call(prefix + ".get", {{ key }}),
    set: (key, value) => call(prefix + ".set", {{ key, value }}),
    remove: (key) => call(prefix + ".remove", {{ key }}),
    keys: () => call(prefix + ".keys"),
  }});
  globalThis.wisp = Object.freeze({{
    plugin: Object.freeze({meta}),
    browser: Object.freeze({{ gpuCompositing: true, android: true }}),
    storage: store("storage"),
    session: store("session"),
    sync: Object.freeze({{
      list: (prefix) => call("sync.list", {{ prefix }}),
      add: (entry) => call("sync.add", {{ entry }}),
      update: (id, fields) => call("sync.update", {{ id, fields }}),
      remove: (id) => call("sync.remove", {{ id }}),
      move: (id, target, after = false) => call("sync.move", {{ id, target, after }}),
      status: () => call("sync.status"),
    }}),
    runtime: Object.freeze({{
      sendMessage: (message) => call("runtime.send", {{ message }}),
      broadcast: (message) => call("runtime.broadcast", {{ message }}),
    }}),
    onMessage: (fn) => {{ handlers.push(fn); }},
    native: Object.freeze({{
      post: (message) => call("native.post", {{ message }}),
      close: () => call("native.close"),
    }}),
    action: Object.freeze({{ setBadge: (text) => call("action.badge", {{ text: String(text || "") }}) }}),
    downloads: Object.freeze({{ start: (url) => call("downloads.start", {{ url }}) }}),
    cookies: Object.freeze({{ netscape: (url) => call("cookies.netscape", {{ url }}) }}),
    openPath: (path) => call("open.path", {{ path }}),
    clipboard: Object.freeze({{ write: (text, {{ secret = false }} = {{}}) => call("clipboard.write", {{ text: String(text), secret: !!secret }}) }}),
    tabs: Object.freeze({{
      open: (url) => call("tabs.open", {{ url }}),
      current: () => call("tabs.current"),
      go: (url) => call("tabs.go", {{ url }}),
    }}),
    notify: (title, body = "") => call("notify", {{ title, body }}),
    log: (...parts) => call("log", {{ message: parts.map(String).join(" ") }}),
    on: (type, fn) => {{ (listeners[type] = listeners[type] || []).push(fn); }},
    _emit: emit,
  }});
}})();
"#
    )
}
