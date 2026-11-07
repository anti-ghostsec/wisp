//! Browsing history and open tabs, both optional and off by default.
//!
//! Stored encrypted (AES-256-GCM) in the data folder, with the vault's key:
//! with a Wisp password set, they can't be read without it. While the vault
//! is locked nothing is recorded, read or written.

use crate::config;
use crate::sync::{decrypt, encrypt};
use crate::vault;
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
    history: RefCell<Option<Vec<Visit>>>,
    save_queued: Cell<bool>,
}

fn history_path() -> PathBuf {
    config::data_dir().join("history.enc")
}

fn session_path() -> PathBuf {
    config::data_dir().join("session.enc")
}

fn now() -> i64 {
    glib::real_time() / 1_000_000
}

impl Store {
    pub fn new() -> Rc<Store> {
        Rc::new(Store { history: RefCell::new(None), save_queued: Cell::new(false) })
    }

    /// The vault's key; None while it's locked.
    fn key(&self) -> Option<[u8; 32]> {
        vault::get().data_key()
    }

    fn read<T: for<'de> Deserialize<'de>>(&self, path: &Path) -> Option<T> {
        let blob = fs::read_to_string(path).ok()?;
        let text = decrypt(&self.key()?, &blob).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn write<T: Serialize>(&self, path: &Path, value: &T) {
        let (Some(key), Ok(text)) = (self.key(), serde_json::to_string(value)) else { return };
        if let Ok(blob) = encrypt(&key, &text) {
            let _ = vault::write_private(path, &blob);
        }
    }

    // ---- history ----

    fn with_history<R>(&self, f: impl FnOnce(&mut Vec<Visit>) -> R) -> R {
        // Locked: what's on disk can't be read, so nothing is kept (and an
        // empty list never gets saved over it).
        if self.key().is_none() {
            return f(&mut Vec::new());
        }
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
