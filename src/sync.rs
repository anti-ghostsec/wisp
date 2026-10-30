//! xBrowserSync client.
//!
//! One encrypted bookmark tree per account, stored on an xBrowserSync service.
//! Plugins never see the account or the crypto: they read and write entries
//! filtered by title prefix (`b_` bookmarks, `p_` passwords, ...).
//!
//! Wire format (compatible with the official xBrowserSync apps, v1.5+):
//!   key  = PBKDF2-HMAC-SHA256(password, salt = sync id, 250 000 rounds, 32 bytes)
//!   blob = base64(iv[16] || AES-256-GCM(key, iv, lzutf8(json)))

use crate::config;
use aes_gcm::aead::consts::U16;
use aes_gcm::aead::{Aead, KeyInit, Nonce};
use aes_gcm::aes::Aes256;
use aes_gcm::AesGcm;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use sha2::Sha256;
use std::cell::{Cell, RefCell};
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};
use webkit6::glib::translate::IntoGlib;
use webkit6::{gio, glib, soup};
use webkit6::soup::prelude::*;

pub const DEFAULT_SERVICE: &str = "https://api.xbrowsersync.org";
const API_VERSION: &str = "1.5.4";
const ITERATIONS: u32 = 250_000;
/// xBrowserSync's default container for bookmarks that aren't on a toolbar/menu.
const OTHER_CONTAINER: &str = "[xbs] Other";
/// How long a pulled copy is trusted before asking the service if it changed.
const FRESH_FOR: Duration = Duration::from_secs(60);

type Cipher = AesGcm<Aes256, U16>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Account {
    pub service: String,
    pub id: String,
    pub password: String,
}

impl Account {
    fn path() -> PathBuf {
        config::config_dir().join("sync.json")
    }

    pub fn load() -> Option<Account> {
        serde_json::from_str(&fs::read_to_string(Self::path()).ok()?).ok()
    }

    fn save(&self) -> Result<(), String> {
        let _ = fs::create_dir_all(config::config_dir());
        let path = Self::path();
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(&path, text).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

/// What this device knows: the tree as last seen on the service, plus writes
/// that haven't been uploaded yet. Plugins see `local` straight away, so reads
/// and writes never wait for the network.
struct State {
    base: Json,
    last_updated: String,
    /// `base` with `pending` applied.
    local: Json,
    /// Not-yet-uploaded writes: {"op": "add"|"update"|"remove", "id", ...}.
    /// Replayed on top of the service's copy if another device changed it.
    pending: Vec<Json>,
    /// When the service was last asked whether the data changed.
    checked: Option<Instant>,
}

pub struct Sync {
    http: soup::Session,
    account: RefCell<Option<Account>>,
    key: RefCell<Option<(Account, [u8; 32])>>,
    state: RefCell<Option<State>>,
    busy: Cell<bool>,
    flush_queued: Cell<bool>,
    refreshing: Cell<bool>,
    listeners: RefCell<Vec<Box<dyn Fn()>>>,
}

impl Sync {
    pub fn new() -> Rc<Sync> {
        let http = soup::Session::new();
        http.set_timeout(20);
        http.set_user_agent("wisp");
        Rc::new(Sync {
            http,
            account: RefCell::new(Account::load()),
            key: RefCell::new(None),
            state: RefCell::new(None),
            busy: Cell::new(false),
            flush_queued: Cell::new(false),
            refreshing: Cell::new(false),
            listeners: RefCell::new(Vec::new()),
        })
    }

    pub fn account(&self) -> Option<Account> {
        self.account.borrow().clone()
    }

    /// Called whenever the entries may have changed (a write here, or new
    /// data from the service).
    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        self.listeners.borrow_mut().push(Box::new(f));
    }

    /// Callers must not hold a borrow of `state`: listeners read entries.
    fn changed(&self) {
        for f in self.listeners.borrow().iter() {
            f();
        }
    }

    /// Writes waiting to be uploaded.
    pub fn pending(&self) -> usize {
        self.state.borrow().as_ref().map_or(0, |s| s.pending.len())
    }

    /// Load the local copy and check the service in the background, so the
    /// first page that needs a login or bookmark doesn't wait.
    pub fn warm_up(self: &Rc<Self>) {
        if self.account().is_none() {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            match this.ensure_loaded().await {
                Ok(()) => this.refresh_in_background(),
                Err(e) => eprintln!("wisp: sync: {e}"),
            }
        });
    }

    // ---- account management ----

    /// Create a fresh sync id with a random password on `service`.
    pub async fn create_account(&self, service: &str) -> Result<Account, String> {
        let service = service.trim_end_matches('/').to_string();
        let info = self.request("GET", &format!("{service}/info"), None).await?;
        if info["status"].as_i64() == Some(3) {
            return Err("this service is not accepting new syncs".into());
        }
        let created = self
            .request("POST", &format!("{service}/bookmarks"), Some(json!({ "version": API_VERSION })))
            .await?;
        let id = created["id"].as_str().ok_or("service did not return a sync id")?.to_string();
        let account = Account { service, id, password: random_password() };
        self.set_account(account.clone())?;
        Ok(account)
    }

    /// Switch to an existing account after checking the password decrypts it.
    pub async fn use_account(&self, account: Account) -> Result<(), String> {
        let account = Account { service: account.service.trim_end_matches('/').to_string(), ..account };
        let key = derive_key(&account).await;
        let data = self
            .request("GET", &format!("{}/bookmarks/{}", account.service, account.id), None)
            .await?;
        let blob = data["bookmarks"].as_str().unwrap_or("");
        if !blob.is_empty() {
            decrypt(&key, blob).map_err(|_| "wrong password for this sync id".to_string())?;
        }
        self.set_account(account.clone())?;
        *self.key.borrow_mut() = Some((account, key));
        Ok(())
    }

    fn set_account(&self, account: Account) -> Result<(), String> {
        account.save()?;
        *self.account.borrow_mut() = Some(account);
        *self.state.borrow_mut() = None;
        *self.key.borrow_mut() = None;
        let _ = fs::remove_file(cache_path());
        Ok(())
    }

    // ---- entries ----

    /// All bookmark entries whose title starts with `prefix`.
    pub async fn list(self: &Rc<Self>, prefix: &str) -> Result<Vec<Json>, String> {
        if self.account().is_none() {
            // Nothing synced yet; don't create an account just to read.
            return Ok(Vec::new());
        }
        self.ensure_loaded().await?;
        let mut out = Vec::new();
        let stale = {
            let state = self.state.borrow();
            let s = state.as_ref().ok_or("sync data unavailable")?;
            collect(&s.local, prefix, &mut out);
            s.checked.is_none_or(|t| t.elapsed() > FRESH_FOR)
        };
        if stale {
            self.refresh_in_background();
        }
        Ok(out)
    }

    /// Entries from the local copy only, without waiting for anything; empty
    /// until it's loaded.
    pub fn cached(&self, prefix: &str) -> Vec<Json> {
        let mut out = Vec::new();
        if let Some(s) = self.state.borrow().as_ref() {
            collect(&s.local, prefix, &mut out);
        }
        out
    }

    /// Add an entry; creates an account on the default service if needed.
    pub async fn add(self: &Rc<Self>, entry: Json) -> Result<i64, String> {
        self.prepare_write().await?;
        let id = {
            let state = self.state.borrow();
            let s = state.as_ref().ok_or("sync data unavailable")?;
            max_id(&s.local).max(max_id(&s.base)) + 1
        };
        self.write(json!({ "op": "add", "id": id, "entry": entry }))?;
        Ok(id)
    }

    pub async fn update(self: &Rc<Self>, id: i64, fields: Json, prefix: &[String]) -> Result<(), String> {
        self.prepare_write().await?;
        self.write(json!({ "op": "update", "id": id, "fields": fields, "prefix": prefix }))
    }

    /// Put entry `id` just before (or after) entry `target`. Order is the
    /// tree's own, so it syncs and other xBrowserSync apps show it too.
    pub async fn move_entry(self: &Rc<Self>, id: i64, target: i64, after: bool, prefix: &[String]) -> Result<(), String> {
        self.prepare_write().await?;
        self.write(json!({ "op": "move", "id": id, "target": target, "after": after, "prefix": prefix }))
    }

    pub async fn remove(self: &Rc<Self>, id: i64, prefix: &[String]) -> Result<(), String> {
        self.prepare_write().await?;
        self.write(json!({ "op": "remove", "id": id, "prefix": prefix }))
    }

    async fn prepare_write(self: &Rc<Self>) -> Result<(), String> {
        if self.account().is_none() {
            self.create_account(DEFAULT_SERVICE).await?;
        }
        self.ensure_loaded().await
    }

    /// Apply a write locally now and upload it shortly after.
    fn write(self: &Rc<Self>, op: Json) -> Result<(), String> {
        {
            let mut state = self.state.borrow_mut();
            let s = state.as_mut().ok_or("sync data unavailable")?;
            apply_op(&mut s.local, &op)?;
            s.pending.push(op);
        }
        self.changed();
        self.schedule_flush();
        Ok(())
    }

    // ---- local copy ----

    async fn ensure_loaded(self: &Rc<Self>) -> Result<(), String> {
        if self.state.borrow().is_some() {
            return Ok(());
        }
        if self.load_cache() {
            self.changed();
            if self.pending() > 0 {
                self.schedule_flush();
            }
            return Ok(());
        }
        let _guard = self.lock().await;
        if self.state.borrow().is_none() {
            self.pull().await?;
        }
        Ok(())
    }

    /// The local copy, encrypted like the service's, plus the derived key so
    /// startup skips the slow key derivation. Readable only by this user,
    /// like sync.json, which holds the password anyway.
    fn load_cache(&self) -> bool {
        let Some(account) = self.account() else { return false };
        let Ok(text) = fs::read_to_string(cache_path()) else { return false };
        let Ok(file) = serde_json::from_str::<Json>(&text) else { return false };
        if file["service"].as_str() != Some(&account.service) || file["id"].as_str() != Some(&account.id) {
            return false;
        }
        let Some(key) = file["key"].as_str().and_then(|k| B64.decode(k).ok()).and_then(|k| <[u8; 32]>::try_from(k).ok())
        else {
            return false;
        };
        let Some(data) = file["data"]
            .as_str()
            .and_then(|b| decrypt(&key, b).ok())
            .and_then(|t| serde_json::from_str::<Json>(&t).ok())
        else {
            return false;
        };
        let base = data["base"].clone();
        let pending = data["pending"].as_array().cloned().unwrap_or_default();
        let (local, pending) = replay(&base, pending);
        *self.key.borrow_mut() = Some((account, key));
        *self.state.borrow_mut() = Some(State {
            base,
            last_updated: data["last_updated"].as_str().unwrap_or("").to_string(),
            local,
            pending,
            checked: None,
        });
        true
    }

    fn save_cache(&self) {
        let Some((account, key)) = self.key.borrow().clone() else { return };
        let data = {
            let state = self.state.borrow();
            let Some(s) = state.as_ref() else { return };
            json!({ "base": s.base, "last_updated": s.last_updated, "pending": s.pending }).to_string()
        };
        let Ok(blob) = encrypt(&key, &data) else { return };
        let file = json!({ "service": account.service, "id": account.id, "key": B64.encode(key), "data": blob });
        let path = cache_path();
        let _ = fs::create_dir_all(config::data_dir());
        let tmp = path.with_extension("tmp");
        if fs::write(&tmp, file.to_string()).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
            }
            let _ = fs::rename(&tmp, &path);
        }
    }

    // ---- talking to the service ----

    fn refresh_in_background(self: &Rc<Self>) {
        if self.refreshing.replace(true) {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let result = {
                let _guard = this.lock().await;
                this.check_remote().await
            };
            this.refreshing.set(false);
            if let Err(e) = result {
                eprintln!("wisp: sync: {e}");
            }
        });
    }

    /// Ask the service whether the data changed (a tiny request) and pull it if so.
    async fn check_remote(&self) -> Result<(), String> {
        let account = self.account().ok_or("no sync account")?;
        let last = self.state.borrow().as_ref().map(|s| s.last_updated.clone());
        if let Some(last) = last {
            let url = format!("{}/bookmarks/{}/lastUpdated", account.service, account.id);
            let remote = self.request("GET", &url, None).await?;
            if remote["lastUpdated"].as_str() == Some(&last) {
                if let Some(s) = self.state.borrow_mut().as_mut() {
                    s.checked = Some(Instant::now());
                }
                return Ok(());
            }
        }
        self.pull().await
    }

    /// Download the service's copy and replay local writes on top of it.
    async fn pull(&self) -> Result<(), String> {
        let account = self.account().ok_or("no sync account")?;
        let data = self.request("GET", &format!("{}/bookmarks/{}", account.service, account.id), None).await?;
        let blob = data["bookmarks"].as_str().unwrap_or("");
        let key = self.key().await?;
        let base: Json = if blob.is_empty() {
            json!([])
        } else {
            serde_json::from_str(&decrypt(&key, blob)?).map_err(|e| format!("sync data is not valid JSON: {e}"))?
        };
        let pending = self.state.borrow_mut().as_mut().map(|s| std::mem::take(&mut s.pending)).unwrap_or_default();
        let (local, pending) = replay(&base, pending);
        let last_updated = data["lastUpdated"].as_str().unwrap_or("").to_string();
        *self.state.borrow_mut() = Some(State { base, last_updated, local, pending, checked: Some(Instant::now()) });
        self.save_cache();
        self.changed();
        Ok(())
    }

    fn schedule_flush(self: &Rc<Self>) {
        if self.flush_queued.replace(true) {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            // Batch writes that come in quick succession (imports, edits).
            glib::timeout_future(Duration::from_millis(300)).await;
            this.flush_queued.set(false);
            this.save_cache();
            let mut wait = 5;
            while let Err(e) = this.flush().await {
                eprintln!("wisp: sync: {e}; retrying in {wait} s");
                glib::timeout_future(Duration::from_secs(wait)).await;
                wait = (wait * 2).min(300);
            }
        });
    }

    /// Upload pending writes, merging with other devices' changes on conflict.
    async fn flush(&self) -> Result<(), String> {
        let _guard = self.lock().await;
        for _ in 0..5 {
            let (tree, last_updated, count) = {
                let state = self.state.borrow();
                let Some(s) = state.as_ref() else { return Ok(()) };
                if s.pending.is_empty() {
                    return Ok(());
                }
                (s.local.clone(), s.last_updated.clone(), s.pending.len())
            };
            match self.push(&tree, &last_updated).await {
                Ok(updated) => {
                    if let Some(s) = self.state.borrow_mut().as_mut() {
                        // Writes made during the upload stay pending.
                        s.pending.drain(..count);
                        s.base = tree;
                        s.last_updated = updated;
                        s.checked = Some(Instant::now());
                    }
                    self.save_cache();
                }
                Err(e) if e.starts_with("409") => self.pull().await?,
                Err(e) => return Err(e),
            }
        }
        Err("sync kept conflicting with another device".into())
    }

    async fn key(&self) -> Result<[u8; 32], String> {
        let account = self.account().ok_or("no sync account")?;
        if let Some((a, k)) = self.key.borrow().as_ref() {
            if *a == account {
                return Ok(*k);
            }
        }
        let key = derive_key(&account).await;
        *self.key.borrow_mut() = Some((account, key));
        Ok(key)
    }

    async fn push(&self, tree: &Json, last_updated: &str) -> Result<String, String> {
        let account = self.account().ok_or("no sync account")?;
        let key = self.key().await?;
        let blob = encrypt(&key, &tree.to_string())?;
        let body = json!({ "bookmarks": blob, "lastUpdated": last_updated });
        let res = self
            .request("PUT", &format!("{}/bookmarks/{}", account.service, account.id), Some(body))
            .await?;
        Ok(res["lastUpdated"].as_str().unwrap_or("").to_string())
    }

    async fn request(&self, method: &str, url: &str, body: Option<Json>) -> Result<Json, String> {
        let msg = soup::Message::new(method, url).map_err(|e| format!("bad URL {url}: {e}"))?;
        if let Some(body) = body {
            let bytes = glib::Bytes::from_owned(body.to_string().into_bytes());
            msg.set_request_body_from_bytes(Some("application/json"), Some(&bytes));
        }
        let bytes = self
            .http
            .send_and_read_future(&msg, glib::Priority::DEFAULT)
            .await
            .map_err(|e| format!("cannot reach sync service: {e}"))?;
        let status = msg.status().into_glib();
        let parsed: Json = serde_json::from_slice(&bytes).unwrap_or(Json::Null);
        if !(200..300).contains(&status) {
            let detail = parsed["message"].as_str().unwrap_or("request failed");
            return Err(format!("{status} {detail}"));
        }
        Ok(parsed)
    }

    /// Serialize operations so concurrent writes don't overwrite each other.
    async fn lock(&self) -> BusyGuard<'_> {
        while self.busy.get() {
            glib::timeout_future(Duration::from_millis(15)).await;
        }
        self.busy.set(true);
        BusyGuard(&self.busy)
    }
}

struct BusyGuard<'a>(&'a Cell<bool>);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

// ---- crypto ----

async fn derive_key(account: &Account) -> [u8; 32] {
    let (password, salt) = (account.password.clone(), account.id.clone());
    // 250k PBKDF2 rounds take a noticeable moment; keep them off the UI thread.
    gio::spawn_blocking(move || {
        let mut key = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), salt.as_bytes(), ITERATIONS, &mut key);
        key
    })
    .await
    .expect("key derivation thread panicked")
}

pub(crate) fn encrypt(key: &[u8; 32], text: &str) -> Result<String, String> {
    let cipher = Cipher::new_from_slice(key).map_err(|e| e.to_string())?;
    let mut iv = [0u8; 16];
    getrandom::fill(&mut iv).map_err(|e| e.to_string())?;
    // Plain UTF-8 is a valid LZUTF8 stream (one with no back-references).
    let sealed = cipher
        .encrypt(&Nonce::<Cipher>::from(iv), text.as_bytes())
        .map_err(|_| "encryption failed".to_string())?;
    let mut out = iv.to_vec();
    out.extend_from_slice(&sealed);
    Ok(B64.encode(out))
}

pub(crate) fn decrypt(key: &[u8; 32], blob: &str) -> Result<String, String> {
    let raw = B64.decode(blob.trim()).map_err(|_| "sync data is not base64".to_string())?;
    if raw.len() < 16 + 16 {
        return Err("sync data is too short".into());
    }
    let cipher = Cipher::new_from_slice(key).map_err(|e| e.to_string())?;
    let nonce = Nonce::<Cipher>::try_from(&raw[..16]).map_err(|_| "bad IV".to_string())?;
    let plain = cipher
        .decrypt(&nonce, &raw[16..])
        .map_err(|_| "cannot decrypt sync data (wrong password?)".to_string())?;
    String::from_utf8(lzutf8_decompress(&plain)).map_err(|_| "sync data is not UTF-8".to_string())
}

/// Decoder for LZUTF8, the compression xBrowserSync applies before encrypting.
/// Back-references are 2- or 3-byte sequences whose lead byte looks like a
/// UTF-8 lead (110xxxxx / 111xxxxx) but whose next byte is not a continuation.
fn lzutf8_decompress(input: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(input.len() * 2);
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        let next_is_continuation = input.get(i + 1).is_some_and(|n| n >> 7 == 1);
        if b >> 6 != 3 || i + 1 >= input.len() || next_is_continuation {
            out.push(b);
            i += 1;
            continue;
        }
        let length = (b & 31) as usize;
        let (distance, used) = if b >> 5 == 6 {
            (input[i + 1] as usize, 2)
        } else if i + 2 < input.len() {
            (((input[i + 1] as usize) << 8) | input[i + 2] as usize, 3)
        } else {
            out.push(b);
            i += 1;
            continue;
        };
        let start = out.len().saturating_sub(distance);
        for k in 0..length {
            let byte = out.get(start + k).copied().unwrap_or(b' ');
            out.push(byte);
        }
        i += used;
    }
    out
}

fn random_password() -> String {
    const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).expect("no OS randomness");
    bytes.iter().map(|b| CHARS[*b as usize % CHARS.len()] as char).collect()
}

// ---- tree helpers ----

fn collect(node: &Json, prefix: &str, out: &mut Vec<Json>) {
    if let Some(items) = node.as_array() {
        for item in items {
            collect(item, prefix, out);
        }
        return;
    }
    if let Some(children) = node.get("children") {
        collect(children, prefix, out);
        return;
    }
    let title = node["title"].as_str().unwrap_or("");
    if node.get("url").is_some() && title.starts_with(prefix) {
        out.push(json!({
            "id": node["id"],
            "title": title,
            "name": &title[prefix.len()..],
            "url": node["url"],
            "description": node.get("description").cloned().unwrap_or(Json::Null),
            "tags": node.get("tags").cloned().unwrap_or(json!([])),
        }));
    }
}

fn max_id(node: &Json) -> i64 {
    match node {
        Json::Array(items) => items.iter().map(max_id).max().unwrap_or(0),
        Json::Object(o) => {
            let own = o.get("id").and_then(Json::as_i64).unwrap_or(0);
            own.max(o.get("children").map(max_id).unwrap_or(0))
        }
        _ => 0,
    }
}

fn other_container(tree: &mut Json) -> &mut Vec<Json> {
    if !tree.is_array() {
        *tree = json!([]);
    }
    let pos = tree.as_array().unwrap().iter().position(|c| c["title"] == OTHER_CONTAINER);
    let pos = match pos {
        Some(p) => p,
        None => {
            let id = max_id(tree) + 1;
            let items = tree.as_array_mut().unwrap();
            items.push(json!({ "id": id, "title": OTHER_CONTAINER, "children": [] }));
            items.len() - 1
        }
    };
    let container = &mut tree.as_array_mut().unwrap()[pos];
    if !container["children"].is_array() {
        container["children"] = json!([]);
    }
    container["children"].as_array_mut().unwrap()
}

fn find_mut(node: &mut Json, id: i64) -> Option<&mut Json> {
    match node {
        Json::Array(items) => items.iter_mut().find_map(|i| find_mut(i, id)),
        Json::Object(_) => {
            if node["id"].as_i64() == Some(id) && node.get("url").is_some() {
                return Some(node);
            }
            node.get_mut("children").and_then(|c| find_mut(c, id))
        }
        _ => None,
    }
}

fn remove_id(node: &mut Json, id: i64) {
    match node {
        Json::Array(items) => {
            items.retain(|i| !(i["id"].as_i64() == Some(id) && i.get("url").is_some()));
            items.iter_mut().for_each(|i| remove_id(i, id));
        }
        Json::Object(o) => {
            if let Some(c) = o.get_mut("children") {
                remove_id(c, id);
            }
        }
        _ => {}
    }
}

fn cache_path() -> PathBuf {
    config::data_dir().join("sync-cache.json")
}

/// Apply one pending write to a tree.
fn apply_op(tree: &mut Json, op: &Json) -> Result<(), String> {
    let id = op["id"].as_i64().ok_or("write without an id")?;
    let prefix: Vec<String> = serde_json::from_value(op["prefix"].clone()).unwrap_or_default();
    match op["op"].as_str() {
        Some("add") => {
            // Create the container first so it can't take the same id.
            other_container(tree);
            let mut item = op["entry"].clone();
            item["id"] = json!(id);
            other_container(tree).push(item);
        }
        Some("update") => {
            let item = find_mut(tree, id).ok_or("no such entry")?;
            check_prefix(item, &prefix)?;
            for key in ["title", "url", "description", "tags"] {
                if let Some(v) = op["fields"].get(key) {
                    item[key] = v.clone();
                }
            }
            check_prefix(item, &prefix)?;
        }
        Some("move") => {
            let target = op["target"].as_i64().ok_or("move without a target")?;
            check_prefix(find_mut(tree, id).ok_or("no such entry")?, &prefix)?;
            check_prefix(find_mut(tree, target).ok_or("no such entry")?, &prefix)?;
            if id == target {
                return Ok(());
            }
            let item = take(tree, id).ok_or("no such entry")?;
            let mut item = Some(item);
            place_near(tree, target, op["after"].as_bool().unwrap_or(false), &mut item);
            if item.is_some() {
                return Err("move target vanished".into());
            }
        }
        Some("remove") => {
            check_prefix(find_mut(tree, id).ok_or("no such entry")?, &prefix)?;
            remove_id(tree, id);
        }
        _ => return Err("unknown write".into()),
    }
    Ok(())
}

/// `base` with `ops` applied. An added entry whose id another device took
/// meanwhile gets a new one; writes that no longer apply are dropped.
fn replay(base: &Json, ops: Vec<Json>) -> (Json, Vec<Json>) {
    let mut tree = base.clone();
    let mut kept = Vec::new();
    let mut moved: Vec<(i64, i64)> = Vec::new();
    for mut op in ops {
        if let Some(&(_, to)) = moved.iter().find(|(from, _)| op["id"].as_i64() == Some(*from)) {
            op["id"] = json!(to);
        }
        if op["op"] == "add" {
            let id = op["id"].as_i64().unwrap_or(0);
            if has_id(&tree, id) {
                let to = max_id(&tree) + 1;
                moved.push((id, to));
                op["id"] = json!(to);
            }
        }
        match apply_op(&mut tree, &op) {
            Ok(()) => kept.push(op),
            Err(e) => eprintln!("wisp: sync: dropped a write that no longer applies: {e}"),
        }
    }
    (tree, kept)
}

/// Remove entry `id` from wherever it is and return it.
fn take(node: &mut Json, id: i64) -> Option<Json> {
    match node {
        Json::Array(items) => {
            if let Some(pos) = items.iter().position(|i| i["id"].as_i64() == Some(id) && i.get("url").is_some()) {
                return Some(items.remove(pos));
            }
            items.iter_mut().find_map(|i| take(i, id))
        }
        Json::Object(o) => o.get_mut("children").and_then(|c| take(c, id)),
        _ => None,
    }
}

/// Insert `item` next to entry `target`, in the same folder. Leaves it in
/// `item` when the target isn't found.
fn place_near(node: &mut Json, target: i64, after: bool, item: &mut Option<Json>) {
    match node {
        Json::Array(items) => {
            if let Some(pos) = items.iter().position(|i| i["id"].as_i64() == Some(target)) {
                if let Some(it) = item.take() {
                    items.insert(if after { pos + 1 } else { pos }, it);
                }
                return;
            }
            for i in items.iter_mut() {
                if item.is_none() {
                    return;
                }
                place_near(i, target, after, item);
            }
        }
        Json::Object(o) => {
            if let Some(c) = o.get_mut("children") {
                place_near(c, target, after, item);
            }
        }
        _ => {}
    }
}

fn has_id(node: &Json, id: i64) -> bool {
    match node {
        Json::Array(items) => items.iter().any(|i| has_id(i, id)),
        Json::Object(o) => o.get("id").and_then(Json::as_i64) == Some(id) || o.get("children").is_some_and(|c| has_id(c, id)),
        _ => false,
    }
}

fn check_prefix(item: &Json, allowed: &[String]) -> Result<(), String> {
    let title = item["title"].as_str().unwrap_or("");
    if allowed.iter().any(|p| title.starts_with(p.as_str())) {
        Ok(())
    } else {
        Err(format!("entry title must start with one of {allowed:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crypto_round_trip() {
        let key = [7u8; 32];
        let blob = encrypt(&key, r#"[{"title":"b_héllo"}]"#).unwrap();
        assert_eq!(decrypt(&key, &blob).unwrap(), r#"[{"title":"b_héllo"}]"#);
        assert!(decrypt(&[8u8; 32], &blob).is_err());
    }

    #[test]
    fn lzutf8_references() {
        // "abcabc": literal "abc" + 2-byte reference (length 3, distance 3).
        assert_eq!(lzutf8_decompress(&[b'a', b'b', b'c', 0b1100_0011, 3]), b"abcabc");
        // Real multi-byte UTF-8 passes through untouched.
        assert_eq!(lzutf8_decompress("é€".as_bytes()), "é€".as_bytes());
        // 3-byte reference (length 2, distance 4).
        assert_eq!(lzutf8_decompress(&[b'w', b'x', b'y', b'z', 0b1110_0010, 0, 4]), b"wxyzwx");
    }

    #[test]
    fn tree_operations() {
        let mut tree = json!([{ "id": 4, "title": "[xbs] Toolbar", "children": [
            { "id": 5, "title": "b_Rust", "url": "https://rust-lang.org" },
            { "id": 6, "title": "Plain", "url": "https://example.com" }
        ]}]);
        let mut out = vec![];
        collect(&tree, "b_", &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["name"], "Rust");
        other_container(&mut tree);
        let next = max_id(&tree) + 1;
        other_container(&mut tree).push(json!({ "id": next, "title": "p_x", "url": "https://x" }));
        assert_eq!(max_id(&tree), 8);
        assert!(check_prefix(find_mut(&mut tree, 8).unwrap(), &["p_".into()]).is_ok());
        assert!(check_prefix(find_mut(&mut tree, 6).unwrap(), &["b_".into()]).is_err());
        remove_id(&mut tree, 5);
        let mut out = vec![];
        collect(&tree, "b_", &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn moving_entries() {
        let px = json!(["b_"]);
        let mut tree = json!([{ "id": 1, "title": "[xbs] Other", "children": [
            { "id": 2, "title": "b_a", "url": "https://a" },
            { "id": 3, "title": "b_b", "url": "https://b" },
            { "id": 4, "title": "b_c", "url": "https://c" },
            { "id": 5, "title": "p_secret", "url": "https://p" }
        ]}]);
        let order = |t: &Json| {
            let mut out = vec![];
            collect(t, "b_", &mut out);
            out.iter().map(|e| e["name"].as_str().unwrap().to_string()).collect::<Vec<_>>().join("")
        };
        apply_op(&mut tree, &json!({ "op": "move", "id": 4, "target": 2, "after": false, "prefix": px })).unwrap();
        assert_eq!(order(&tree), "cab");
        apply_op(&mut tree, &json!({ "op": "move", "id": 4, "target": 3, "after": true, "prefix": px })).unwrap();
        assert_eq!(order(&tree), "abc");
        // Can't move, or move next to, another plugin's entries.
        assert!(apply_op(&mut tree, &json!({ "op": "move", "id": 5, "target": 2, "prefix": px })).is_err());
        assert!(apply_op(&mut tree, &json!({ "op": "move", "id": 2, "target": 5, "prefix": px })).is_err());
    }

    #[test]
    fn replay_moves_clashing_ids() {
        let px = json!(["p_"]);
        let ops = vec![
            json!({ "op": "add", "id": 3, "entry": { "title": "p_a", "url": "https://a" } }),
            json!({ "op": "update", "id": 3, "fields": { "description": "x" }, "prefix": px }),
            json!({ "op": "remove", "id": 99, "prefix": px }),
        ];
        // Another device added id 3 meanwhile.
        let base = json!([{ "id": 1, "title": "[xbs] Other", "children": [
            { "id": 2, "title": "p_old", "url": "https://old" },
            { "id": 3, "title": "b_theirs", "url": "https://theirs" }
        ]}]);
        let (tree, kept) = replay(&base, ops);
        assert_eq!(kept.len(), 2);
        let mut out = vec![];
        collect(&tree, "p_", &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1]["id"], 4);
        assert_eq!(out[1]["description"], "x");
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Decrypts the live sync record and prints entry titles (never descriptions).
    /// Run: WISP_BLOB_FILE=blob.json cargo test -- --ignored live_titles --nocapture
    #[test]
    #[ignore]
    fn live_titles() {
        let account = Account::load().expect("no sync.json");
        let raw = fs::read_to_string(std::env::var("WISP_BLOB_FILE").unwrap()).unwrap();
        let data: Json = serde_json::from_str(&raw).unwrap();
        let mut key = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<Sha256>(account.password.as_bytes(), account.id.as_bytes(), ITERATIONS, &mut key);
        let tree: Json = serde_json::from_str(&decrypt(&key, data["bookmarks"].as_str().unwrap()).unwrap()).unwrap();
        for prefix in ["b_", "p_"] {
            let mut out = vec![];
            collect(&tree, prefix, &mut out);
            for e in out {
                let d: Json = serde_json::from_str(e["description"].as_str().unwrap_or("{}")).unwrap_or_default();
                println!("{prefix} id={} title={} url={} user_len={:?} pw_len={:?}", e["id"], e["title"], e["url"],
                    d["username"].as_str().map(str::len), d["password"].as_str().map(str::len));
            }
        }
    }
}
