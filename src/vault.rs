//! The vault: the key that encrypts what Wisp keeps (history, open tabs,
//! plugin data, the local copy of bookmarks and passwords) and the sync
//! account's login, in one sealed file, `~/.config/wisp/vault.json`.
//!
//! With a Wisp password (asked for on first start) it's sealed with a key
//! made from that password by Argon2id, and Wisp asks for it once each time
//! it starts: nothing on disk can be read without it, not even by someone
//! with the whole home folder. Without one it's sealed with a key kept
//! beside it, which only stops it being read by accident.
//!
//! Until it's unlocked Wisp runs as if nothing were kept: no history is
//! recorded, no tabs saved, and bookmarks and passwords can't be read or
//! changed (so nothing on disk is overwritten while it can't be read).

use crate::config;
use crate::sync::{Account, decrypt, encrypt};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use webkit6::{gio, glib};

/// What a call that needs the vault says while it's locked.
pub const LOCKED: &str = "Wisp is locked: unlock it in ☰ → Wisp password to use saved bookmarks and passwords";

/// Argon2id: 64 MiB, 3 passes. About half a second, once per start.
const MEMORY_KIB: u32 = 64 * 1024;
const PASSES: u32 = 3;
const LANES: u32 = 1;

/// The file as it is on disk.
#[derive(Clone, Serialize, Deserialize)]
struct Sealed {
    version: u32,
    /// "argon2id" (a password) or "none".
    kdf: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    salt: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    m_kib: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    t: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    p: u32,
    /// The sealing key itself, only when there's no password.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    kek: String,
    /// `Secrets` as JSON, encrypted (AES-256-GCM) with the sealing key.
    secrets: String,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// What the vault keeps.
#[derive(Clone, Serialize, Deserialize)]
struct Secrets {
    /// The data key, base64.
    key: String,
    #[serde(default)]
    sync: Option<Account>,
}

pub struct Vault {
    sealed: RefCell<Option<Sealed>>,
    /// Once unlocked: what's in it, and the key it's sealed with.
    open: RefCell<Option<(Secrets, [u8; 32])>>,
}

thread_local! {
    static VAULT: Rc<Vault> = Rc::new(Vault::load());
}

/// The vault (one per Wisp, on the UI thread).
pub fn get() -> Rc<Vault> {
    VAULT.with(Rc::clone)
}

fn vault_path() -> PathBuf {
    config::config_dir().join("vault.json")
}

/// Where older Wisps kept these, unsealed.
fn legacy_key_path() -> PathBuf {
    config::config_dir().join("state.key")
}
fn legacy_sync_path() -> PathBuf {
    config::config_dir().join("sync.json")
}
pub fn plugin_data_dir() -> PathBuf {
    config::data_dir().join("plugin-data")
}

/// Write `text` readable only by you, in one step.
pub fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let result = (|| {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        std::io::Write::write_all(&mut f, text.as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Overwrite a file with zeros before removing it, so what it held isn't
/// simply left in the free space (on an SSD the drive may still keep old
/// copies; full-disk encryption is what covers those).
pub fn wipe(path: &Path) {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.is_file()
            && let Ok(mut f) = fs::OpenOptions::new().write(true).open(path)
        {
            let _ = std::io::Write::write_all(&mut f, &vec![0u8; meta.len() as usize]);
            let _ = f.sync_all();
        }
        let _ = fs::remove_file(path);
    }
}

fn random_key() -> Result<[u8; 32], String> {
    let mut k = [0u8; 32];
    getrandom::fill(&mut k).map_err(|e| e.to_string())?;
    Ok(k)
}

fn decode_key(text: &str) -> Option<[u8; 32]> {
    B64.decode(text.trim()).ok().and_then(|b| <[u8; 32]>::try_from(b).ok())
}

fn argon2(password: &str, salt: &[u8], m_kib: u32, t: u32, p: u32) -> Result<[u8; 32], String> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(m_kib, t, p, Some(32)).map_err(|e| e.to_string())?;
    let mut out = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// Argon2 off the UI thread: it takes a moment and 64 MiB.
async fn derive(password: String, salt: Vec<u8>, m_kib: u32, t: u32, p: u32) -> Result<[u8; 32], String> {
    gio::spawn_blocking(move || argon2(&password, &salt, m_kib, t, p)).await.map_err(|_| "key derivation failed".to_string())?
}

/// A sealing key and the header that goes with it: from a password, or a
/// random one stored in the file.
async fn new_seal(password: Option<String>) -> Result<(Sealed, [u8; 32]), String> {
    match password {
        Some(pw) => {
            let mut salt = [0u8; 16];
            getrandom::fill(&mut salt).map_err(|e| e.to_string())?;
            let kek = derive(pw, salt.to_vec(), MEMORY_KIB, PASSES, LANES).await?;
            let header = Sealed {
                version: 1,
                kdf: "argon2id".into(),
                salt: B64.encode(salt),
                m_kib: MEMORY_KIB,
                t: PASSES,
                p: LANES,
                kek: String::new(),
                secrets: String::new(),
            };
            Ok((header, kek))
        }
        None => {
            let kek = random_key()?;
            let header =
                Sealed { version: 1, kdf: "none".into(), salt: String::new(), m_kib: 0, t: 0, p: 0, kek: B64.encode(kek), secrets: String::new() };
            Ok((header, kek))
        }
    }
}

impl Vault {
    fn load() -> Vault {
        let sealed: Option<Sealed> = fs::read_to_string(vault_path()).ok().and_then(|t| serde_json::from_str(&t).ok());
        let vault = Vault { sealed: RefCell::new(sealed), open: RefCell::new(None) };
        vault.open_unprotected();
        vault
    }

    /// Without a password the vault opens by itself.
    fn open_unprotected(&self) {
        let Some(s) = self.sealed.borrow().clone() else { return };
        if s.kdf != "none" {
            return;
        }
        if let Some(kek) = decode_key(&s.kek)
            && let Ok(secrets) = unseal(&kek, &s.secrets)
        {
            *self.open.borrow_mut() = Some((secrets, kek));
        }
    }

    /// The vault has been set up (with a password or without).
    pub fn exists(&self) -> bool {
        self.sealed.borrow().is_some()
    }

    /// It's locked with a password.
    pub fn protected(&self) -> bool {
        self.sealed.borrow().as_ref().is_some_and(|s| s.kdf != "none")
    }

    pub fn is_open(&self) -> bool {
        self.open.borrow().is_some()
    }

    /// Wisp should ask about the vault before it opens a window: to set it
    /// up, or to unlock it.
    pub fn wants_password(&self) -> bool {
        !self.exists() || !self.is_open()
    }

    /// The key for history, tabs, plugin data and the bookmark cache; None
    /// while locked.
    pub fn data_key(&self) -> Option<[u8; 32]> {
        self.open.borrow().as_ref().and_then(|(s, _)| decode_key(&s.key))
    }

    pub fn account(&self) -> Option<Account> {
        self.open.borrow().as_ref().and_then(|(s, _)| s.sync.clone())
    }

    pub fn set_account(&self, account: Account) -> Result<(), String> {
        {
            let mut open = self.open.borrow_mut();
            let (secrets, _) = open.as_mut().ok_or(LOCKED)?;
            secrets.sync = Some(account);
        }
        self.save()
    }

    pub async fn unlock(&self, password: &str) -> Result<(), String> {
        let s = self.sealed.borrow().clone().ok_or("no vault yet")?;
        if s.kdf != "argon2id" {
            return Err(format!("unknown kind of vault: {}", s.kdf));
        }
        let salt = B64.decode(&s.salt).map_err(|_| "vault is damaged".to_string())?;
        let kek = derive(password.to_string(), salt, s.m_kib, s.t, s.p).await?;
        let secrets = unseal(&kek, &s.secrets).map_err(|_| "Wrong password.".to_string())?;
        *self.open.borrow_mut() = Some((secrets, kek));
        Ok(())
    }

    /// Set the vault up, taking in what older Wisps kept unsealed: the key
    /// history and tabs were encrypted with, the sync login, and plugin
    /// data. Those old files are wiped.
    pub async fn create(&self, password: Option<String>) -> Result<(), String> {
        if self.exists() {
            return Err("there is a vault already".into());
        }
        let legacy_key = fs::read_to_string(legacy_key_path()).ok().and_then(|t| decode_key(&t));
        let key = match legacy_key {
            Some(k) => k,
            None => random_key()?,
        };
        let sync = fs::read_to_string(legacy_sync_path()).ok().and_then(|t| serde_json::from_str::<Account>(&t).ok());
        let secrets = Secrets { key: B64.encode(key), sync };
        let (mut header, kek) = new_seal(password).await?;
        header.secrets = seal(&kek, &secrets)?;
        write_private(&vault_path(), &serde_json::to_string_pretty(&header).map_err(|e| e.to_string())?)
            .map_err(|e| format!("couldn't save the vault: {e}"))?;
        *self.sealed.borrow_mut() = Some(header);
        *self.open.borrow_mut() = Some((secrets, kek));
        seal_plugin_data(&key);
        wipe(&legacy_key_path());
        wipe(&legacy_sync_path());
        Ok(())
    }

    /// A new password (or none). Only while unlocked.
    pub async fn set_password(&self, password: Option<String>) -> Result<(), String> {
        let secrets = self.open.borrow().as_ref().map(|(s, _)| s.clone()).ok_or(LOCKED)?;
        let (mut header, kek) = new_seal(password).await?;
        header.secrets = seal(&kek, &secrets)?;
        write_private(&vault_path(), &serde_json::to_string_pretty(&header).map_err(|e| e.to_string())?)
            .map_err(|e| format!("couldn't save the vault: {e}"))?;
        *self.sealed.borrow_mut() = Some(header);
        *self.open.borrow_mut() = Some((secrets, kek));
        Ok(())
    }

    fn save(&self) -> Result<(), String> {
        let (secrets, kek) = self.open.borrow().clone().ok_or(LOCKED)?;
        let mut header = self.sealed.borrow().clone().ok_or("no vault yet")?;
        header.secrets = seal(&kek, &secrets)?;
        write_private(&vault_path(), &serde_json::to_string_pretty(&header).map_err(|e| e.to_string())?)
            .map_err(|e| format!("couldn't save the vault: {e}"))?;
        *self.sealed.borrow_mut() = Some(header);
        Ok(())
    }
}

fn seal(kek: &[u8; 32], secrets: &Secrets) -> Result<String, String> {
    encrypt(kek, &serde_json::to_string(secrets).map_err(|e| e.to_string())?)
}

fn unseal(kek: &[u8; 32], blob: &str) -> Result<Secrets, String> {
    serde_json::from_str(&decrypt(kek, blob)?).map_err(|e| e.to_string())
}

/// Plugin data left unencrypted by older Wisps, encrypted in place.
fn seal_plugin_data(key: &[u8; 32]) {
    let Ok(dir) = fs::read_dir(plugin_data_dir()) else { return };
    for entry in dir.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else { continue };
        if !is_plain_json(&text) {
            continue;
        }
        if let Ok(blob) = encrypt(key, &text) {
            // A new file in its place (the rename), then the old one's
            // contents are gone with it.
            let _ = write_private(&path, &blob);
        }
    }
}

/// Plugin data from before it was encrypted is JSON as it is; encrypted
/// data is base64, which never starts with `{`.
pub fn is_plain_json(text: &str) -> bool {
    text.trim_start().starts_with('{')
}

// ---- asking for the password ----

/// What `ask` calls when it's done.
type Done = Box<dyn FnOnce(bool)>;

#[derive(Clone, Copy, PartialEq)]
pub enum Ask {
    /// First start: choose a password (or none).
    Setup,
    /// Enter it.
    Unlock,
    /// Change it, or remove it.
    Change,
}

/// The vault's window. `done(true)` once the vault is open (set up, unlocked
/// or changed), `done(false)` if put off; called once.
pub fn ask(app: &gtk::Application, parent: Option<&gtk::Window>, kind: Ask, done: impl FnOnce(bool) + 'static) {
    let vault = get();
    let window = gtk::Window::builder().application(app).title("Wisp password").default_width(440).resizable(false).build();
    if let Some(p) = parent {
        window.set_transient_for(Some(p));
        window.set_modal(true);
    }
    let done: Rc<RefCell<Option<Done>>> = Rc::new(RefCell::new(Some(Box::new(done))));
    let finish = {
        let (done, window) = (done.clone(), window.clone());
        move |ok: bool| {
            // Taken first: closing the window comes back here.
            let f = done.borrow_mut().take();
            if let Some(f) = f {
                window.close();
                f(ok);
            }
        }
    };
    {
        let finish = finish.clone();
        window.connect_close_request(move |_| {
            finish(false);
            glib::Propagation::Proceed
        });
    }

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 10);
    vbox.set_margin_top(18);
    vbox.set_margin_bottom(18);
    vbox.set_margin_start(18);
    vbox.set_margin_end(18);
    let (title, text) = match kind {
        Ask::Setup => (
            "Protect what Wisp keeps",
            "Saved passwords, bookmarks, history and open tabs are kept encrypted. Choose a password \
             to lock them: Wisp asks for it each time it starts, and without it they can't be read, \
             even by someone with your files. It can't be recovered if you forget it.",
        ),
        Ask::Unlock => ("Wisp is locked", "Enter your Wisp password to use your saved passwords, bookmarks, history and tabs."),
        Ask::Change if vault.protected() => {
            ("Wisp password", "Choose a new password, or remove it (then what Wisp keeps is only encrypted with a key stored beside it).")
        }
        Ask::Change => (
            "Wisp password",
            "What Wisp keeps isn't locked with a password: anyone who can read your files can read it. Choose one to lock it.",
        ),
    };
    vbox.append(&gtk::Label::builder().label(format!("<b>{title}</b>")).use_markup(true).xalign(0.0).build());
    vbox.append(&gtk::Label::builder().label(text).wrap(true).xalign(0.0).max_width_chars(52).build());

    let password = gtk::PasswordEntry::builder().show_peek_icon(true).hexpand(true).activates_default(true).build();
    let confirm = gtk::PasswordEntry::builder().show_peek_icon(true).hexpand(true).activates_default(true).build();
    let grid = gtk::Grid::builder().row_spacing(6).column_spacing(10).build();
    let label = if kind == Ask::Unlock { "Password" } else { "New password" };
    grid.attach(&gtk::Label::builder().label(label).xalign(1.0).build(), 0, 0, 1, 1);
    grid.attach(&password, 1, 0, 1, 1);
    if kind != Ask::Unlock {
        grid.attach(&gtk::Label::builder().label("Again").xalign(1.0).build(), 0, 1, 1, 1);
        grid.attach(&confirm, 1, 1, 1, 1);
    }
    vbox.append(&grid);
    let status = gtk::Label::builder().wrap(true).xalign(0.0).max_width_chars(52).build();
    status.add_css_class("dim-label");
    vbox.append(&status);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    buttons.set_halign(gtk::Align::End);
    let other = match kind {
        Ask::Setup => Some("Don't use a password"),
        Ask::Change if vault.protected() => Some("Remove password"),
        _ => None,
    }
    .map(gtk::Button::with_label);
    let later = gtk::Button::with_label(if kind == Ask::Change { "Close" } else { "Not now" });
    let main = gtk::Button::with_label(match kind {
        Ask::Setup => "Use this password",
        Ask::Unlock => "Unlock",
        Ask::Change if vault.protected() => "Change password",
        Ask::Change => "Set password",
    });
    main.add_css_class("suggested-action");
    if let Some(b) = &other {
        buttons.append(b);
    }
    // At first start, putting it off would just ask again next time.
    if kind != Ask::Setup {
        buttons.append(&later);
    }
    buttons.append(&main);
    vbox.append(&buttons);
    window.set_child(Some(&vbox));
    window.set_default_widget(Some(&main));

    let busy = {
        let (main, other, later) = (main.clone(), other.clone(), later.clone());
        move |on: bool| {
            main.set_sensitive(!on);
            later.set_sensitive(!on);
            if let Some(o) = &other {
                o.set_sensitive(!on);
            }
        }
    };

    {
        let finish = finish.clone();
        later.connect_clicked(move |_| finish(false));
    }
    if let Some(other) = &other {
        let (finish, status, busy) = (finish.clone(), status.clone(), busy.clone());
        other.connect_clicked(move |_| {
            let (finish, status, busy) = (finish.clone(), status.clone(), busy.clone());
            busy(true);
            glib::spawn_future_local(async move {
                let vault = get();
                let result = if kind == Ask::Setup { vault.create(None).await } else { vault.set_password(None).await };
                match result {
                    Ok(()) => finish(true),
                    Err(e) => {
                        status.set_text(&e);
                        busy(false);
                    }
                }
            });
        });
    }
    main.connect_clicked(move |_| {
        let (pw, again) = (password.text().to_string(), confirm.text().to_string());
        if pw.is_empty() {
            status.set_text("Enter a password.");
            return;
        }
        if kind != Ask::Unlock && pw != again {
            status.set_text("The two passwords aren't the same.");
            return;
        }
        let (finish, status, busy, password) = (finish.clone(), status.clone(), busy.clone(), password.clone());
        busy(true);
        status.set_text(if kind == Ask::Unlock { "Unlocking…" } else { "Locking…" });
        glib::spawn_future_local(async move {
            let vault = get();
            let result = match kind {
                Ask::Setup => vault.create(Some(pw)).await,
                Ask::Unlock => vault.unlock(&pw).await,
                Ask::Change => vault.set_password(Some(pw)).await,
            };
            match result {
                Ok(()) => finish(true),
                Err(e) => {
                    status.set_text(&e);
                    password.set_text("");
                    password.grab_focus();
                    busy(false);
                }
            }
        });
    });
    window.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_secrets_need_the_right_key() {
        let kek = argon2("correct horse", b"0123456789abcdef", 64, 1, 1).unwrap();
        let wrong = argon2("wrong horse", b"0123456789abcdef", 64, 1, 1).unwrap();
        let secrets = Secrets {
            key: B64.encode([7u8; 32]),
            sync: Some(Account { service: "https://example.org".into(), id: "abc".into(), password: "hunter2".into() }),
        };
        let blob = seal(&kek, &secrets).unwrap();
        assert!(!blob.contains("hunter2") && !blob.contains("abc"));
        assert!(unseal(&wrong, &blob).is_err());
        let back = unseal(&kek, &blob).unwrap();
        assert_eq!(back.sync.unwrap().password, "hunter2");
        assert_eq!(decode_key(&back.key), Some([7u8; 32]));
    }

    #[test]
    fn same_password_same_key_other_salt_other_key() {
        let a = argon2("pw", b"saltsaltsaltsalt", 64, 1, 1).unwrap();
        let b = argon2("pw", b"saltsaltsaltsalt", 64, 1, 1).unwrap();
        let c = argon2("pw", b"SALTSALTSALTSALT", 64, 1, 1).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn plain_and_encrypted_plugin_data_told_apart() {
        assert!(is_plain_json("{\"used\":{}}"));
        assert!(is_plain_json("  {}"));
        let blob = encrypt(&[1u8; 32], "{\"a\":1}").unwrap();
        assert!(!is_plain_json(&blob));
    }

    #[test]
    fn wipe_removes_the_file() {
        let dir = std::env::temp_dir().join(format!("wisp-vault-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("secret");
        write_private(&f, "secret stuff").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        }
        wipe(&f);
        assert!(!f.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
