//! Browsing history and open tabs, both optional and off by default.
//!
//! Stored encrypted (AES-256-GCM) in the data folder. The key is kept apart
//! from the data, in the config folder, readable only by you: copying or
//! backing up the data folder alone reveals nothing. Anyone who can read
//! both folders as you can decrypt them, so full-disk encryption is what
//! protects them from someone with the drive.

use crate::config;
use crate::sync::{decrypt, encrypt};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;
use webkit6::glib;

/// Oldest visits are forgotten past this many pages.
const MAX_HISTORY: usize = 20_000;

#[derive(Clone, Serialize, Deserialize)]
pub struct Visit {
    pub url: String,
    pub title: String,
    pub visits: u32,
    /// Unix time of the last visit, in seconds.
    pub last: i64,
}

/// A saved tab: its address and title.
pub type SavedTab = (String, String);

pub struct Store {
    key: RefCell<Option<[u8; 32]>>,
    history: RefCell<Option<Vec<Visit>>>,
    save_queued: Cell<bool>,
}

fn history_path() -> PathBuf {
    config::data_dir().join("history.enc")
}

fn session_path() -> PathBuf {
    config::data_dir().join("session.enc")
}

fn key_path() -> PathBuf {
    config::config_dir().join("state.key")
}

fn write_private(path: &Path, text: &str) {
    let _ = path.parent().map(fs::create_dir_all);
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, text).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
        }
        let _ = fs::rename(&tmp, path);
    }
}

fn now() -> i64 {
    glib::real_time() / 1_000_000
}

impl Store {
    pub fn new() -> Rc<Store> {
        Rc::new(Store { key: RefCell::new(None), history: RefCell::new(None), save_queued: Cell::new(false) })
    }

    /// The key, made on first use.
    fn key(&self) -> Option<[u8; 32]> {
        if let Some(k) = *self.key.borrow() {
            return Some(k);
        }
        let stored = fs::read_to_string(key_path()).ok().and_then(|t| B64.decode(t.trim()).ok()).and_then(|b| <[u8; 32]>::try_from(b).ok());
        let key = match stored {
            Some(k) => k,
            None => {
                let mut k = [0u8; 32];
                getrandom::fill(&mut k).ok()?;
                write_private(&key_path(), &B64.encode(k));
                k
            }
        };
        *self.key.borrow_mut() = Some(key);
        Some(key)
    }

    fn read<T: for<'de> Deserialize<'de>>(&self, path: &Path) -> Option<T> {
        let blob = fs::read_to_string(path).ok()?;
        let text = decrypt(&self.key()?, &blob).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn write<T: Serialize>(&self, path: &Path, value: &T) {
        let (Some(key), Ok(text)) = (self.key(), serde_json::to_string(value)) else { return };
        if let Ok(blob) = encrypt(&key, &text) {
            write_private(path, &blob);
        }
    }

    // ---- history ----

    fn with_history<R>(&self, f: impl FnOnce(&mut Vec<Visit>) -> R) -> R {
        let mut h = self.history.borrow_mut();
        let list = h.get_or_insert_with(|| self.read(&history_path()).unwrap_or_default());
        f(list)
    }

    pub fn record(self: &Rc<Self>, url: &str, title: &str) {
        self.with_history(|list| {
            match list.iter_mut().find(|v| v.url == url) {
                Some(v) => {
                    v.visits += 1;
                    v.last = now();
                    if !title.is_empty() {
                        v.title = title.to_string();
                    }
                }
                None => list.push(Visit { url: url.into(), title: title.into(), visits: 1, last: now() }),
            }
            if list.len() > MAX_HISTORY {
                list.sort_by_key(|v| std::cmp::Reverse(v.last));
                list.truncate(MAX_HISTORY);
            }
        });
        self.save_soon();
    }

    pub fn set_title(self: &Rc<Self>, url: &str, title: &str) {
        let changed = self.with_history(|list| match list.iter_mut().find(|v| v.url == url) {
            Some(v) if v.title != title && !title.is_empty() => {
                v.title = title.to_string();
                true
            }
            _ => false,
        });
        if changed {
            self.save_soon();
        }
    }

    /// Newest first.
    pub fn history(&self) -> Vec<Visit> {
        let mut list = self.with_history(|l| l.clone());
        list.sort_by_key(|v| std::cmp::Reverse(v.last));
        list
    }

    pub fn clear_history(&self) {
        *self.history.borrow_mut() = Some(Vec::new());
        let _ = fs::remove_file(history_path());
    }

    fn save_soon(self: &Rc<Self>) {
        if self.save_queued.replace(true) {
            return;
        }
        let me = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_secs(2), move || {
            if let Some(me) = me.upgrade() {
                me.save_now();
            }
        });
    }

    /// Write pending history right away (also used when Wisp quits).
    pub fn save_now(&self) {
        self.save_queued.set(false);
        if let Some(list) = self.history.borrow().as_ref() {
            self.write(&history_path(), list);
        }
    }

    // ---- open tabs ----

    pub fn save_session(&self, windows: &[Vec<SavedTab>]) {
        self.write(&session_path(), &windows);
    }

    pub fn load_session(&self) -> Vec<Vec<SavedTab>> {
        self.read(&session_path()).unwrap_or_default()
    }

    pub fn clear_session(&self) {
        let _ = fs::remove_file(session_path());
    }
}
