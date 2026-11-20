//! Signed plugins: with `plugin_signing_key` set, a plugin of your own runs
//! only if its folder is signed with that key (minisign). What's signed is
//! the list of its files and their SHA-256 sums, as `sha256sum` writes it,
//! in `plugin.minisig` (PLUGINS.md says how to make one).

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use blake2::{Blake2b512, Digest};
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::Sha256;
use std::fs;
use std::path::Path;

/// The signature, in the plugin's folder.
pub const SIGNATURE_FILE: &str = "plugin.minisig";

/// Whether the plugin in `dir` is signed with `public_key` (a minisign
/// public key: its base64 line, or the whole .pub file).
pub fn check_plugin(dir: &Path, public_key: &str) -> Result<(), String> {
    let sig = fs::read_to_string(dir.join(SIGNATURE_FILE)).map_err(|_| format!("it has no {SIGNATURE_FILE}"))?;
    verify(public_key, contents(dir)?.as_bytes(), &sig)
}

/// "<sha256>  <path>" for each file in the folder but the signature, sorted
/// by path, as `find . -type f -printf '%P\0' | LC_ALL=C sort -z | xargs -0
/// sha256sum` writes them. A link anywhere in it is refused: what it points
/// at could change without the signature noticing.
pub fn contents(dir: &Path) -> Result<String, String> {
    let mut files = Vec::new();
    walk(dir, "", &mut files)?;
    files.sort();
    let mut out = String::new();
    for rel in files {
        let data = fs::read(dir.join(&rel)).map_err(|e| format!("{rel}: {e}"))?;
        let sum: String = Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
        out.push_str(&format!("{sum}  {rel}\n"));
    }
    Ok(out)
}

fn walk(dir: &Path, prefix: &str, files: &mut Vec<String>) -> Result<(), String> {
    let entries = fs::read_dir(dir.join(prefix)).map_err(|e| e.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().into_string().map_err(|_| "a file name isn't UTF-8".to_string())?;
        if name.contains(['\n', '\\']) {
            return Err(format!("the file name {name:?} can't be signed"));
        }
        let rel = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            return Err(format!("{rel} is a link"));
        } else if kind.is_dir() {
            walk(dir, &rel, files)?;
        } else if kind.is_file() && rel != SIGNATURE_FILE {
            files.push(rel);
        }
    }
    Ok(())
}

/// The base64 line of a minisign public key (or .pub file): its key id and
/// the Ed25519 key.
fn public_key(text: &str) -> Result<([u8; 8], VerifyingKey), String> {
    let line = text.lines().map(str::trim).rfind(|l| !l.is_empty() && !l.starts_with("untrusted comment:"));
    let raw = line.and_then(|l| B64.decode(l).ok()).filter(|r| r.len() == 42 && &r[..2] == b"Ed");
    let raw = raw.ok_or("plugin_signing_key isn't a minisign public key")?;
    let id: [u8; 8] = raw[2..10].try_into().unwrap();
    let key: [u8; 32] = raw[10..].try_into().unwrap();
    Ok((id, VerifyingKey::from_bytes(&key).map_err(|_| "plugin_signing_key isn't a valid key".to_string())?))
}

/// Check a minisign signature (`sig`, the .minisig file) of `message`:
/// the signature itself, and the one over it and its trusted comment.
pub fn verify(public: &str, message: &[u8], sig: &str) -> Result<(), String> {
    let (key_id, key) = public_key(public)?;
    let bad = || "its signature is damaged".to_string();
    let mut lines = sig.lines();
    let _untrusted = lines.next().ok_or_else(bad)?;
    let raw = B64.decode(lines.next().ok_or_else(bad)?.trim()).map_err(|_| bad())?;
    let trusted = lines.next().and_then(|l| l.strip_prefix("trusted comment: ")).ok_or_else(bad)?;
    let global = B64.decode(lines.next().ok_or_else(bad)?.trim()).map_err(|_| bad())?;
    if raw.len() != 74 || global.len() != 64 {
        return Err(bad());
    }
    if raw[2..10] != key_id {
        return Err("it's signed with another key".into());
    }
    let signature = Signature::from_slice(&raw[10..]).map_err(|_| bad())?;
    // "ED": the message's BLAKE2b-512 is signed (minisign's default);
    // "Ed": the message itself.
    let checked = match &raw[..2] {
        b"ED" => key.verify_strict(Blake2b512::digest(message).as_slice(), &signature),
        b"Ed" => key.verify_strict(message, &signature),
        _ => return Err(bad()),
    };
    checked.map_err(|_| "its files don't match its signature".to_string())?;
    let mut signed = raw[10..].to_vec();
    signed.extend_from_slice(trusted.as_bytes());
    let global = Signature::from_slice(&global).map_err(|_| bad())?;
    key.verify_strict(&signed, &global).map_err(|_| "its signature's comment was changed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A key made for this test only (minisign -G -W), and the plugin below
    // signed with it by the commands in PLUGINS.md.
    const PUBLIC: &str =
        "untrusted comment: minisign public key D2BF2967F8E7B9B1\nRWSxuef4Zym/0hYiAFCyYOS1i+9VvdGfLqD0hTwdPv0Lx5uziJWQ9P06\n";
    const SIG: &str = "untrusted comment: signature from minisign secret key
RUSxuef4Zym/0nXUmHUB/v60l3SbxTqM7JLlbT8ub19HT6Pm6oKFYkJ4yEsQqK1Eb3YfZnFXiKzXzbYAswc/Omtff61TZRnpgQs=
trusted comment: test plugin
a+FDbil4pa55J3bjFjaLsv3D0dh1ePiTg7yxxhqcl7SPIdg2AfpbpQFBoVvN5puEU2zqFy8daUSu/TAebHqPDg==
";

    fn plugin(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
        fs::create_dir_all(dir.join("icons")).unwrap();
        fs::write(dir.join("plugin.json"), "{\"id\": \"t\", \"name\": \"T\"}\n").unwrap();
        fs::write(dir.join("a.js"), "console.log(1);\n").unwrap();
        fs::write(dir.join("icons/i.svg"), "<svg/>\n").unwrap();
        fs::write(dir.join(SIGNATURE_FILE), SIG).unwrap();
    }

    #[test]
    fn signed_plugins_are_checked() {
        let dir = std::env::temp_dir().join(format!("wisp-signing-test-{}", std::process::id()));
        plugin(&dir);
        assert_eq!(check_plugin(&dir, PUBLIC), Ok(()));
        assert_eq!(check_plugin(&dir, PUBLIC.lines().nth(1).unwrap()), Ok(()));
        // A changed file, an added one, a link.
        fs::write(dir.join("a.js"), "console.log(2);\n").unwrap();
        assert!(check_plugin(&dir, PUBLIC).is_err());
        plugin(&dir);
        fs::write(dir.join("b.js"), "").unwrap();
        assert!(check_plugin(&dir, PUBLIC).is_err());
        plugin(&dir);
        std::os::unix::fs::symlink("/etc/hostname", dir.join("c.js")).unwrap();
        assert!(check_plugin(&dir, PUBLIC).is_err());
        // Another key; no signature; a changed trusted comment.
        plugin(&dir);
        let other = "RWQ4QO7IuKYQ7bxAF8XK3hUgG/iEllb2z0x43I7+Q9AcVQKkzC7u27d9";
        assert!(check_plugin(&dir, other).is_err());
        let mut lines: Vec<String> = SIG.lines().map(str::to_owned).collect();
        lines[2].push('!');
        fs::write(dir.join(SIGNATURE_FILE), lines.join("\n")).unwrap();
        assert!(check_plugin(&dir, PUBLIC).is_err());
        fs::remove_file(dir.join(SIGNATURE_FILE)).unwrap();
        assert!(check_plugin(&dir, PUBLIC).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
