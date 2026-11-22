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
use crate::sync::{Account, decrypt, decrypt_aad, encrypt, encrypt_aad};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// What a call that needs the vault says while it's locked.
pub const LOCKED: &str = "Wisp is locked: unlock it in ☰ → Wisp password to use saved bookmarks and passwords";

/// Argon2id: 64 MiB, 3 passes. About half a second, once per start.
const MEMORY_KIB: u32 = 64 * 1024;
const PASSES: u32 = 3;
const LANES: u32 = 1;

/// What a vault's header may ask of Argon2. Wisp only ever writes the
/// values above, but the header is read before anything proves it's ours:
/// a changed file must not be able to ask for all the memory there is, or
/// for an hour of passes.
const MEMORY_KIB_RANGE: std::ops::RangeInclusive<u32> = 8 * 1024..=256 * 1024;
const PASSES_RANGE: std::ops::RangeInclusive<u32> = 1..=16;
const LANES_RANGE: std::ops::RangeInclusive<u32> = 1..=4;
const SALT_BYTES_RANGE: std::ops::RangeInclusive<usize> = 8..=64;
/// A vault file is a few hundred bytes; nothing bigger is read.
const MAX_VAULT_BYTES: u64 = 64 * 1024;

fn check_kdf(m_kib: u32, t: u32, p: u32, salt_len: usize) -> Result<(), String> {
    if MEMORY_KIB_RANGE.contains(&m_kib) && PASSES_RANGE.contains(&t) && LANES_RANGE.contains(&p) && SALT_BYTES_RANGE.contains(&salt_len) {
        Ok(())
    } else {
        Err("The vault file is damaged or was changed: its settings aren't ones Wisp writes.".into())
    }
}

/// Overwrite `bytes` with zeros in a way the compiler can't drop as
/// "never read again".
fn wipe_bytes(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        // SAFETY: a valid, exclusive reference to one byte.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// The same for a string (a password, a key in base64); it's left empty.
fn wipe_string(text: &mut String) {
    // SAFETY: zeros are valid UTF-8.
    wipe_bytes(unsafe { text.as_bytes_mut() });
    text.clear();
}

/// A key that is wiped from memory when it's dropped.
#[derive(Clone, PartialEq)]
struct Key([u8; 32]);

impl Drop for Key {
    fn drop(&mut self) {
        wipe_bytes(&mut self.0);
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(..)")
    }
}

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
    /// The data key from before the password was last changed, kept only
    /// until every file is sealed with the new one (`rekey`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_key: Option<String>,
    /// The version each data file was last written at, by its name in the
    /// data folder: an older copy put back in its place is refused.
    #[serde(default)]
    versions: BTreeMap<String, u64>,
}

impl Drop for Secrets {
    fn drop(&mut self) {
        wipe_string(&mut self.key);
        if let Some(k) = &mut self.previous_key {
            wipe_string(k);
        }
        if let Some(account) = &mut self.sync {
            wipe_string(&mut account.password);
        }
    }
}

pub struct Vault {
    sealed: RefCell<Option<Sealed>>,
    /// Once unlocked: what's in it, and the key it's sealed with.
    open: RefCell<Option<(Secrets, Key)>>,
    /// The file says "no password" though this vault had one, and it wasn't
    /// Wisp that removed it (see `protection_lost`).
    downgraded: Cell<bool>,
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
/// Whether the vault has a password, as Wisp last left it: "password" or
/// "none". Kept apart from the vault, with Wisp's data and not its
/// settings, so that swapping vault.json for one without a password (whose
/// key whoever swapped it knows) doesn't go unnoticed. Someone who can
/// change both files isn't stopped by it.
fn marker_path() -> PathBuf {
    config::data_dir().join("vault-protection")
}

fn remember_protection(protected: bool) {
    let text = if protected { "password\n" } else { "none\n" };
    if fs::read_to_string(marker_path()).is_ok_and(|now| now == text) {
        return;
    }
    if let Err(e) = write_private(&marker_path(), text) {
        eprintln!("wisp: cannot write {}: {e}", marker_path().display());
    }
}

/// The file has no password, but the vault was last left with one.
fn protection_lost(kdf: &str, marker: Option<&str>) -> bool {
    kdf == "none" && marker.map(str::trim) == Some("password")
}

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

fn random_key() -> Result<Key, String> {
    let mut k = Key([0u8; 32]);
    getrandom::fill(&mut k.0).map_err(|e| e.to_string())?;
    Ok(k)
}

fn decode_key(text: &str) -> Option<[u8; 32]> {
    let mut bytes = B64.decode(text.trim()).ok()?;
    let key = <[u8; 32]>::try_from(bytes.as_slice()).ok();
    wipe_bytes(&mut bytes);
    key
}

fn argon2(password: &str, salt: &[u8], m_kib: u32, t: u32, p: u32) -> Result<Key, String> {
    use argon2::{Algorithm, Argon2, Params, Version};
    check_kdf(m_kib, t, p, salt.len())?;
    let params = Params::new(m_kib, t, p, Some(32)).map_err(|e| e.to_string())?;
    let mut out = Key([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut out.0)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// Argon2 off the UI thread: it takes a moment and 64 MiB. The password is
/// wiped once the key is made.
async fn derive(mut password: String, salt: Vec<u8>, m_kib: u32, t: u32, p: u32) -> Result<Key, String> {
    crate::rt::spawn_blocking(move || {
        let key = argon2(&password, &salt, m_kib, t, p);
        wipe_string(&mut password);
        key
    })
    .await
    .map_err(|_| "key derivation failed".to_string())?
}

/// A sealing key and the header that goes with it: from a password, or a
/// random one stored in the file.
async fn new_seal(password: Option<String>) -> Result<(Sealed, Key), String> {
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
                Sealed { version: 1, kdf: "none".into(), salt: String::new(), m_kib: 0, t: 0, p: 0, kek: B64.encode(kek.0), secrets: String::new() };
            Ok((header, kek))
        }
    }
}

impl Vault {
    fn load() -> Vault {
        let small = fs::metadata(vault_path()).is_ok_and(|m| m.len() <= MAX_VAULT_BYTES);
        let sealed: Option<Sealed> =
            if small { fs::read_to_string(vault_path()).ok().and_then(|t| serde_json::from_str(&t).ok()) } else { None };
        let marker = fs::read_to_string(marker_path()).ok();
        let downgraded = sealed.as_ref().is_some_and(|s| protection_lost(&s.kdf, marker.as_deref()));
        let vault = Vault { sealed: RefCell::new(sealed), open: RefCell::new(None), downgraded: Cell::new(downgraded) };
        // A vault that lost its password behind Wisp's back isn't opened
        // until the user has been told (`warn_downgraded`).
        if !downgraded {
            vault.open_unprotected();
        }
        vault
    }

    /// Lock a vault that has a password again: its key and the sync login
    /// are wiped from memory (they're wiped as they're dropped) until it's
    /// unlocked. A vault without a password stays open: there's nothing to
    /// ask for.
    pub fn lock(&self) {
        if self.protected() {
            *self.open.borrow_mut() = None;
        }
    }

    /// The vault had a password and now the file has none, without Wisp
    /// having removed it: someone or something else replaced the file.
    pub fn downgraded(&self) -> bool {
        self.downgraded.get()
    }

    /// Use the vault without a password after all (the user said so).
    pub fn accept_downgrade(&self) {
        self.downgraded.set(false);
        remember_protection(false);
        self.open_unprotected();
    }

    /// Without a password the vault opens by itself.
    fn open_unprotected(&self) {
        let Some(s) = self.sealed.borrow().clone() else { return };
        if s.kdf != "none" {
            return;
        }
        if let Some(kek) = decode_key(&s.kek).map(Key)
            && let Ok(secrets) = unseal(&kek, &s.secrets)
        {
            *self.open.borrow_mut() = Some((secrets, kek));
            self.rekey();
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
        // Before any memory is set aside for it.
        check_kdf(s.m_kib, s.t, s.p, salt.len())?;
        let kek = derive(password.to_string(), salt, s.m_kib, s.t, s.p).await?;
        let secrets = unseal(&kek, &s.secrets).map_err(|_| "Wrong password.".to_string())?;
        *self.open.borrow_mut() = Some((secrets, kek));
        remember_protection(true);
        self.rekey();
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
            Some(k) => Key(k),
            None => random_key()?,
        };
        let sync = fs::read_to_string(legacy_sync_path()).ok().and_then(|t| serde_json::from_str::<Account>(&t).ok());
        let secrets = Secrets { key: B64.encode(key.0), sync, previous_key: None, versions: BTreeMap::new() };
        let protected = password.is_some();
        let (mut header, kek) = new_seal(password).await?;
        header.secrets = seal(&kek, &secrets)?;
        write_private(&vault_path(), &serde_json::to_string_pretty(&header).map_err(|e| e.to_string())?)
            .map_err(|e| format!("couldn't save the vault: {e}"))?;
        *self.sealed.borrow_mut() = Some(header);
        *self.open.borrow_mut() = Some((secrets, kek));
        remember_protection(protected);
        self.seal_plain_plugin_data();
        wipe(&legacy_key_path());
        wipe(&legacy_sync_path());
        Ok(())
    }

    /// A new password (or none). Only while unlocked. It comes with a new
    /// data key too, and everything kept is sealed again with it: someone
    /// who knew the old password, or kept a copy of the old vault file,
    /// can't read what's kept from now on.
    pub async fn set_password(&self, password: Option<String>) -> Result<(), String> {
        let mut secrets = self.open.borrow().as_ref().map(|(s, _)| s.clone()).ok_or(LOCKED)?;
        let protected = password.is_some();
        let (mut header, kek) = new_seal(password).await?;
        // A change left half done keeps the key from before that one.
        if secrets.previous_key.is_none() {
            secrets.previous_key = Some(secrets.key.clone());
        }
        secrets.key = B64.encode(random_key()?.0);
        header.secrets = seal(&kek, &secrets)?;
        // Before the file changes: if Wisp stops in between, a vault with a
        // password and a note saying "none" is no trouble, the other way
        // round is a false alarm.
        if !protected {
            remember_protection(false);
        }
        write_private(&vault_path(), &serde_json::to_string_pretty(&header).map_err(|e| e.to_string())?)
            .map_err(|e| format!("couldn't save the vault: {e}"))?;
        *self.sealed.borrow_mut() = Some(header);
        *self.open.borrow_mut() = Some((secrets, kek));
        remember_protection(protected);
        self.rekey();
        Ok(())
    }

    /// A new data key, and everything kept sealed again with it, keeping the
    /// password: what was deleted before (cleared history, older copies of
    /// files the disk may still hold) can't be opened with the key Wisp
    /// holds from now on. Only while unlocked.
    pub fn renew_data_key(&self) -> Result<(), String> {
        let (mut secrets, kek) = self.open.borrow().as_ref().map(|(s, k)| (s.clone(), k.clone())).ok_or(LOCKED)?;
        let mut header = self.sealed.borrow().clone().ok_or(LOCKED)?;
        if secrets.previous_key.is_none() {
            secrets.previous_key = Some(secrets.key.clone());
        }
        secrets.key = B64.encode(random_key()?.0);
        header.secrets = seal(&kek, &secrets)?;
        write_private(&vault_path(), &serde_json::to_string_pretty(&header).map_err(|e| e.to_string())?)
            .map_err(|e| format!("couldn't save the vault: {e}"))?;
        *self.sealed.borrow_mut() = Some(header);
        *self.open.borrow_mut() = Some((secrets, kek));
        self.rekey();
        Ok(())
    }

    /// Seal `text` with the data key for the file at `path`, and write it
    /// there (as `{"sealed": ...}` when `wrapped`). The file's name and a
    /// new version are bound to it, so it can't be passed off as another
    /// file, or as an older copy of itself.
    pub fn write_sealed(&self, path: &Path, text: &str, wrapped: bool) -> Result<(), String> {
        self.write_version(path, text, wrapped)?;
        self.save()
    }

    /// `write_sealed` without saving the vault after.
    fn write_version(&self, path: &Path, text: &str, wrapped: bool) -> Result<(), String> {
        let name = data_name(path);
        let (key, version) = {
            let open = self.open.borrow();
            let (s, _) = open.as_ref().ok_or(LOCKED)?;
            (decode_key(&s.key).ok_or("the vault is damaged")?, s.versions.get(&name).copied().unwrap_or(0) + 1)
        };
        let blob = seal_data(&key, &name, version, text)?;
        let body = if wrapped { json!({ "sealed": blob }).to_string() } else { blob };
        write_private(path, &body).map_err(|e| e.to_string())?;
        // Only once the file is written: if Wisp stops in between, the
        // file is newer than the vault says, which is let through.
        if let Some((s, _)) = self.open.borrow_mut().as_mut() {
            s.versions.insert(name, version);
        }
        Ok(())
    }

    /// What `write_sealed` wrote. None while locked, if it's damaged or for
    /// another file, or if it's older than the last version written (a
    /// copy from before put back).
    pub fn read_sealed(&self, path: &Path, wrapped: bool) -> Option<String> {
        let text = fs::read_to_string(path).ok()?;
        let blob = if wrapped { serde_json::from_str::<Json>(&text).ok()?.get("sealed")?.as_str()?.to_string() } else { text };
        let name = data_name(path);
        let open = self.open.borrow();
        let (s, _) = open.as_ref()?;
        let keys: Vec<[u8; 32]> = [Some(&s.key), s.previous_key.as_ref()].into_iter().flatten().filter_map(|k| decode_key(k)).collect();
        match open_data(&keys, &name, s.versions.get(&name).copied(), &blob) {
            Ok(text) => Some(text),
            Err(e) => {
                eprintln!("wisp: not reading {}: {e}", path.display());
                None
            }
        }
    }

    /// After a password change: every data file sealed again with the new
    /// data key, then the old one forgotten. Run again on the next start if
    /// Wisp stopped before it was done.
    fn rekey(&self) {
        if self.open.borrow().as_ref().is_none_or(|(s, _)| s.previous_key.is_none()) {
            return;
        }
        for (path, wrapped) in data_files() {
            if !path.exists() {
                continue;
            }
            let plain = if !wrapped && fs::read_to_string(&path).is_ok_and(|t| is_plain_json(&t)) {
                fs::read_to_string(&path).ok()
            } else {
                self.read_sealed(&path, wrapped)
            };
            let Some(mut text) = plain else {
                eprintln!("wisp: {} can't be read, so it's left as it is", path.display());
                continue;
            };
            let written = self.write_version(&path, &text, wrapped);
            wipe_string(&mut text);
            if let Err(e) = written {
                // The old key stays until it's done; the next start tries again.
                eprintln!("wisp: cannot seal {} again: {e}", path.display());
                let _ = self.save();
                return;
            }
        }
        if let Some((s, _)) = self.open.borrow_mut().as_mut()
            && let Some(mut old) = s.previous_key.take()
        {
            wipe_string(&mut old);
        }
        if let Err(e) = self.save() {
            eprintln!("wisp: {e}");
        }
    }

    /// Plugin data left unencrypted by older Wisps, encrypted in place.
    fn seal_plain_plugin_data(&self) {
        for (path, _) in data_files().into_iter().filter(|(p, _)| p.starts_with(plugin_data_dir())) {
            let Ok(text) = fs::read_to_string(&path) else { continue };
            if is_plain_json(&text) {
                // A new file in its place (the rename), then the old one's
                // contents are gone with it.
                let _ = self.write_version(&path, &text, false);
            }
        }
        let _ = self.save();
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

fn seal(kek: &Key, secrets: &Secrets) -> Result<String, String> {
    let mut json = serde_json::to_string(secrets).map_err(|e| e.to_string())?;
    let sealed = encrypt(&kek.0, &json);
    wipe_string(&mut json);
    sealed
}

fn unseal(kek: &Key, blob: &str) -> Result<Secrets, String> {
    let mut json = decrypt(&kek.0, blob)?;
    let secrets = serde_json::from_str(&json).map_err(|e| e.to_string());
    wipe_string(&mut json);
    secrets
}

/// What a data file sealed by `write_sealed` holds: `wisp2:`, the version,
/// `:`, then the sealed text. Older Wisps wrote the sealed text alone.
const DATA_PREFIX: &str = "wisp2:";

/// A data file's name as bound to what it holds: its path in the data folder.
fn data_name(path: &Path) -> String {
    path.strip_prefix(config::data_dir()).unwrap_or(path).to_string_lossy().into_owned()
}

/// What's bound to a data file's sealed text besides the key.
fn data_aad(name: &str, version: u64) -> Vec<u8> {
    format!("wisp data 2\0{name}\0{version}").into_bytes()
}

fn seal_data(key: &[u8; 32], name: &str, version: u64, text: &str) -> Result<String, String> {
    Ok(format!("{DATA_PREFIX}{version}:{}", encrypt_aad(key, text, &data_aad(name, version))?))
}

/// The text of a data file named `name`, with the first of `keys` that
/// opens it. `last` is the version last written: an older one is refused,
/// and so is one in the old format once a versioned one was written.
fn open_data(keys: &[[u8; 32]], name: &str, last: Option<u64>, blob: &str) -> Result<String, String> {
    let blob = blob.trim();
    match blob.strip_prefix(DATA_PREFIX) {
        Some(rest) => {
            let (version, sealed) = rest.split_once(':').ok_or("damaged")?;
            let version: u64 = version.parse().map_err(|_| "damaged")?;
            if last.is_some_and(|last| version < last) {
                return Err(format!("it's version {version}, older than the last one written ({}): a copy from before?", last.unwrap_or(0)));
            }
            keys.iter()
                .find_map(|k| decrypt_aad(k, sealed, &data_aad(name, version)).ok())
                .ok_or_else(|| "it can't be opened with this vault's key, or it's another file's".to_string())
        }
        // From before files had versions: read as it is, and written with
        // one the next time.
        None if last.is_some() => Err("it's in the old format, though a newer one was written".into()),
        None => keys.iter().find_map(|k| decrypt(k, blob).ok()).ok_or_else(|| "it can't be opened with this vault's key".to_string()),
    }
}

/// Every file sealed with the data key, and whether it's `{"sealed": ...}`.
fn data_files() -> Vec<(PathBuf, bool)> {
    let mut files = vec![
        (crate::state::history_path(), false),
        (crate::state::session_path(), false),
        (crate::sync::local_path(), true),
        (crate::sync::cache_path(), true),
    ];
    if let Ok(dir) = fs::read_dir(plugin_data_dir()) {
        let mut plugin: Vec<PathBuf> =
            dir.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
        plugin.sort();
        files.extend(plugin.into_iter().map(|p| (p, false)));
    }
    files
}

/// Plugin data from before it was encrypted is JSON as it is; encrypted
/// data is base64, which never starts with `{`.
pub fn is_plain_json(text: &str) -> bool {
    text.trim_start().starts_with('{')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The least memory a vault may ask for: quick enough for tests.
    const SMALL: u32 = *MEMORY_KIB_RANGE.start();

    #[test]
    fn a_header_cannot_ask_for_too_much_or_too_little() {
        assert!(check_kdf(MEMORY_KIB, PASSES, LANES, 16).is_ok());
        assert!(check_kdf(SMALL, 1, 1, 8).is_ok());
        assert!(check_kdf(256 * 1024, 16, 4, 64).is_ok());
        // All the memory there is, hours of passes, or next to no work.
        assert!(check_kdf(u32::MAX, 3, 1, 16).is_err());
        assert!(check_kdf(256 * 1024 + 1, 3, 1, 16).is_err());
        assert!(check_kdf(MEMORY_KIB, u32::MAX, 1, 16).is_err());
        assert!(check_kdf(MEMORY_KIB, 17, 1, 16).is_err());
        assert!(check_kdf(MEMORY_KIB, 3, 5, 16).is_err());
        assert!(check_kdf(8, 1, 1, 16).is_err());
        assert!(check_kdf(MEMORY_KIB, 0, 1, 16).is_err());
        assert!(check_kdf(MEMORY_KIB, 3, 0, 16).is_err());
        assert!(check_kdf(MEMORY_KIB, 3, 1, 0).is_err());
        assert!(check_kdf(MEMORY_KIB, 3, 1, 65).is_err());
        // Refused before Argon2 sets any memory aside.
        assert!(argon2("pw", b"saltsaltsaltsalt", u32::MAX, 3, 1).is_err());
    }

    #[test]
    fn a_vault_that_lost_its_password_is_noticed() {
        assert!(protection_lost("none", Some("password\n")));
        // Removed in Wisp, never had one, or an older Wisp that kept no note.
        assert!(!protection_lost("none", Some("none\n")));
        assert!(!protection_lost("none", None));
        assert!(!protection_lost("argon2id", Some("password\n")));
        assert!(!protection_lost("argon2id", Some("none\n")));
    }

    #[test]
    fn keys_and_passwords_are_wiped() {
        let mut bytes = [7u8; 32];
        wipe_bytes(&mut bytes);
        assert_eq!(bytes, [0u8; 32]);
        let mut text = String::from("hunter2");
        wipe_string(&mut text);
        assert!(text.is_empty());
        // What a dropped key leaves behind is zeros.
        let mut slot = std::mem::ManuallyDrop::new(Key([9u8; 32]));
        // SAFETY: dropped once, and only its plain bytes are looked at after.
        unsafe { std::mem::ManuallyDrop::drop(&mut slot) };
        assert_eq!(slot.0, [0u8; 32]);
    }

    #[test]
    fn sealed_secrets_need_the_right_key() {
        let kek = argon2("correct horse", b"0123456789abcdef", SMALL, 1, 1).unwrap();
        let wrong = argon2("wrong horse", b"0123456789abcdef", SMALL, 1, 1).unwrap();
        let secrets = Secrets {
            key: B64.encode([7u8; 32]),
            sync: Some(Account { service: "https://example.org".into(), id: "abc".into(), password: "hunter2".into() }),
            previous_key: None,
            versions: BTreeMap::new(),
        };
        let blob = seal(&kek, &secrets).unwrap();
        assert!(!blob.contains("hunter2") && !blob.contains("abc"));
        assert!(unseal(&wrong, &blob).is_err());
        let back = unseal(&kek, &blob).unwrap();
        assert_eq!(back.sync.as_ref().unwrap().password, "hunter2");
        assert_eq!(decode_key(&back.key), Some([7u8; 32]));
    }

    #[test]
    fn same_password_same_key_other_salt_other_key() {
        let a = argon2("pw", b"saltsaltsaltsalt", SMALL, 1, 1).unwrap();
        let b = argon2("pw", b"saltsaltsaltsalt", SMALL, 1, 1).unwrap();
        let c = argon2("pw", b"SALTSALTSALTSALT", SMALL, 1, 1).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn data_is_bound_to_its_file_and_version() {
        let key = [3u8; 32];
        let blob = seal_data(&key, "history.enc", 5, "[1,2]").unwrap();
        assert!(blob.starts_with("wisp2:5:"));
        assert_eq!(open_data(&[key], "history.enc", Some(5), &blob).unwrap(), "[1,2]");
        // Newer than the vault says (Wisp stopped before saving it): fine.
        assert_eq!(open_data(&[key], "history.enc", Some(4), &blob).unwrap(), "[1,2]");
        // Passed off as another file, or with its version changed.
        assert!(open_data(&[key], "session.enc", Some(5), &blob).is_err());
        let bumped = blob.replacen("wisp2:5:", "wisp2:9:", 1);
        assert!(open_data(&[key], "history.enc", Some(5), &bumped).is_err());
        // An older copy put back.
        assert!(open_data(&[key], "history.enc", Some(6), &blob).is_err());
        // Another key.
        assert!(open_data(&[[4u8; 32]], "history.enc", Some(5), &blob).is_err());
    }

    #[test]
    fn old_format_is_read_until_a_versioned_one_is_written() {
        let key = [3u8; 32];
        let old = encrypt(&key, "[1]").unwrap();
        assert_eq!(open_data(&[key], "history.enc", None, &old).unwrap(), "[1]");
        assert!(open_data(&[key], "history.enc", Some(1), &old).is_err());
    }

    #[test]
    fn data_from_before_a_password_change_opens_with_the_previous_key() {
        let (new, old) = ([5u8; 32], [6u8; 32]);
        let blob = seal_data(&old, "plugin-data/passwords.json", 2, "{}").unwrap();
        assert!(open_data(&[new], "plugin-data/passwords.json", Some(2), &blob).is_err());
        assert_eq!(open_data(&[new, old], "plugin-data/passwords.json", Some(2), &blob).unwrap(), "{}");
    }

    #[test]
    fn old_vaults_still_read() {
        let kek = argon2("pw", b"0123456789abcdef", SMALL, 1, 1).unwrap();
        let blob = encrypt(&kek.0, &format!("{{\"key\":\"{}\"}}", B64.encode([7u8; 32]))).unwrap();
        let back = unseal(&kek, &blob).unwrap();
        assert!(back.versions.is_empty() && back.previous_key.is_none());
        assert_eq!(decode_key(&back.key), Some([7u8; 32]));
    }

    /// Runs a future that never waits (nothing here needs a runtime).
    fn now<T>(f: impl std::future::Future<Output = T>) -> T {
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let mut f = std::pin::pin!(f);
        match f.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(v) => v,
            std::task::Poll::Pending => panic!("it waited"),
        }
    }

    #[test]
    fn a_password_change_seals_everything_again_with_a_new_key() {
        let root = std::env::temp_dir().join(format!("wisp-vault-rekey-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        config::set_dirs(config::Dirs { config: root.join("config"), data: root.join("data"), cache: root.join("cache") });
        // Never near the real folders, should another test have set them first.
        if config::data_dir() != root.join("data") {
            return;
        }
        let vault = Vault { sealed: RefCell::new(None), open: RefCell::new(None), downgraded: Cell::new(false) };
        now(vault.create(None)).unwrap();
        let history = crate::state::history_path();
        let local = crate::sync::local_path();
        vault.write_sealed(&history, "[\"h\"]", false).unwrap();
        vault.write_sealed(&local, "{\"t\":1}", true).unwrap();
        let stale = fs::read_to_string(&history).unwrap();
        vault.write_sealed(&history, "[\"h2\"]", false).unwrap();
        let before = vault.data_key().unwrap();

        now(vault.set_password(None)).unwrap();
        let after = vault.data_key().unwrap();
        assert_ne!(before, after);
        assert!(vault.open.borrow().as_ref().unwrap().0.previous_key.is_none());
        assert_eq!(vault.read_sealed(&history, false).unwrap(), "[\"h2\"]");
        assert_eq!(vault.read_sealed(&local, true).unwrap(), "{\"t\":1}");
        // Sealed with the new key alone now.
        let blob = fs::read_to_string(&history).unwrap();
        assert!(open_data(&[before], "history.enc", None, &blob).is_err());
        // An older copy put back is refused.
        fs::write(&history, stale).unwrap();
        assert!(vault.read_sealed(&history, false).is_none());

        // A new data key alone (history cleared): what's kept still reads,
        // with the new key only, and the vault file opens with it.
        let old_local = fs::read_to_string(&local).unwrap();
        vault.renew_data_key().unwrap();
        let renewed = vault.data_key().unwrap();
        assert_ne!(renewed, after);
        assert!(vault.open.borrow().as_ref().unwrap().0.previous_key.is_none());
        assert_eq!(vault.read_sealed(&local, true).unwrap(), "{\"t\":1}");
        let blob = fs::read_to_string(&local).unwrap();
        assert_ne!(blob, old_local);
        let reopened = Vault::load();
        assert_eq!(reopened.data_key(), Some(renewed));
        let _ = fs::remove_dir_all(&root);
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
