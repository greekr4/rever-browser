//! Import cookies from a locally installed Chrome on macOS, for the storage
//! panel's "import browser login" action (`src/main` had this as the Electron
//! `browserImport` host). The decryption is the correctness-critical part — a
//! wrong key or padding fails silently — so [`derive_key`] and [`decrypt_value`]
//! are pure and locked by a round-trip test. The orchestration shells out to
//! `security` (keychain) and `sqlite3` (the cookies DB), 0 extra dependencies.
//!
//! macOS only for now. Linux (Secret Service) and Windows (DPAPI) later.

use std::path::{Path, PathBuf};
use std::process::Command;

use aes::Aes128;
use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use sha1::Sha1;

type Decryptor = cbc::Decryptor<Aes128>;

/// Chrome's AES-128-CBC IV is 16 spaces.
const IV: [u8; 16] = [0x20; 16];

/// A decrypted cookie ready to inject via the engine's `set_cookie`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportedCookie {
    pub host: String,
    pub name: String,
    pub value: String,
    pub path: String,
    /// Unix epoch seconds; `None` for a session cookie.
    pub expires: Option<f64>,
    pub secure: bool,
    pub http_only: bool,
}

/// The outcome of an import: the cookies that decrypted, plus counts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportSummary {
    pub cookies: Vec<ImportedCookie>,
    pub undecryptable: u64,
    pub total: u64,
}

/// Derive the AES-128 key from the keychain "Chrome Safe Storage" password:
/// PBKDF2-HMAC-SHA1, salt `saltysalt`, 1003 rounds, 16-byte key.
pub fn derive_key(keychain_password: &[u8]) -> [u8; 16] {
    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<Sha1>(keychain_password, b"saltysalt", 1003, &mut key);
    key
}

/// Decrypt one `encrypted_value`. `v10`/`v11`-prefixed values are AES-128-CBC;
/// anything else is treated as a plaintext value (older Chrome).
pub fn decrypt_value(encrypted: &[u8], key: &[u8; 16]) -> Result<String, String> {
    let body = match encrypted.get(..3) {
        Some(b"v10") | Some(b"v11") => &encrypted[3..],
        // Not an encrypted blob: an old plaintext value stored as-is.
        _ => return Ok(String::from_utf8_lossy(encrypted).into_owned()),
    };
    let plain = Decryptor::new(key.into(), &IV.into())
        .decrypt_padded_vec_mut::<Pkcs7>(body)
        .map_err(|e| format!("AES decrypt failed: {e}"))?;
    Ok(String::from_utf8_lossy(&plain).into_owned())
}

/// Chrome stores `expires_utc` as microseconds since 1601-01-01; convert to
/// Unix epoch seconds. 0 (session cookie) → `None`.
fn chrome_time_to_unix(expires_utc: i64) -> Option<f64> {
    if expires_utc == 0 {
        return None;
    }
    // 11_644_473_600 seconds between 1601-01-01 and 1970-01-01.
    Some(expires_utc as f64 / 1_000_000.0 - 11_644_473_600.0)
}

/// The keychain "Chrome Safe Storage" password (macOS).
fn keychain_password() -> Result<Vec<u8>, String> {
    let out = Command::new("security")
        .args(["find-generic-password", "-s", "Chrome Safe Storage", "-w"])
        .output()
        .map_err(|e| format!("could not run `security`: {e}"))?;
    if !out.status.success() {
        return Err("no \"Chrome Safe Storage\" password in the keychain (is Chrome installed and unlocked?)".to_owned());
    }
    // `-w` prints the password then a trailing newline.
    let mut pw = out.stdout;
    if pw.last() == Some(&b'\n') {
        pw.pop();
    }
    Ok(pw)
}

/// `~/Library/Application Support/Google/Chrome/<profile>/Cookies`.
fn cookies_db(profile: Option<&str>) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(Path::new(&home)
        .join("Library/Application Support/Google/Chrome")
        .join(profile.unwrap_or("Default"))
        .join("Cookies"))
}

/// One raw cookie row read from the DB, before decryption.
struct Row {
    host: String,
    name: String,
    encrypted: Vec<u8>,
    path: String,
    expires_utc: i64,
    secure: bool,
    http_only: bool,
}

/// Read the cookies table via the `sqlite3` CLI, hex-encoding the binary
/// `encrypted_value` so it survives the text output. `immutable=1` reads past
/// Chrome's lock without copying the file.
fn read_rows(db: &Path, hosts: Option<&[String]>) -> Result<Vec<Row>, String> {
    if !db.exists() {
        return Err(format!("no Chrome cookies DB at {}", db.display()));
    }
    // shortcut: immutable=1 reads a live/locked DB without a snapshot copy;
    // switch to copying Cookies+Cookies-wal if a WAL write races a read.
    let uri = format!("file:{}?mode=ro&immutable=1", db.display());
    // Unit separator between columns, record separator between rows — neither
    // appears in cookie fields, so no field needs escaping.
    let sql = "SELECT host_key, name, hex(encrypted_value), path, expires_utc, is_secure, is_httponly FROM cookies";
    let out = Command::new("sqlite3")
        .args(["-separator", "\x1f", "-newline", "\x1e", &uri, sql])
        .output()
        .map_err(|e| format!("could not run `sqlite3`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "sqlite3 failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut rows = Vec::new();
    for record in text.split('\x1e').filter(|r| !r.is_empty()) {
        let cols: Vec<&str> = record.split('\x1f').collect();
        let [host, name, hex_val, path, expires, secure, http_only] = cols[..] else {
            continue; // malformed row
        };
        if let Some(hosts) = hosts
            && !hosts.iter().any(|h| host.contains(h.as_str()))
        {
            continue;
        }
        rows.push(Row {
            host: host.to_owned(),
            name: name.to_owned(),
            encrypted: decode_hex(hex_val),
            path: path.to_owned(),
            expires_utc: expires.trim().parse().unwrap_or(0),
            secure: secure.trim() != "0",
            http_only: http_only.trim() != "0",
        });
    }
    Ok(rows)
}

fn decode_hex(hex: &str) -> Vec<u8> {
    let bytes = hex.trim().as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i + 1 < bytes.len() {
        if let (Some(hi), Some(lo)) = (
            (bytes[i] as char).to_digit(16),
            (bytes[i + 1] as char).to_digit(16),
        ) {
            out.push((hi * 16 + lo) as u8);
        }
        i += 2;
    }
    out
}

/// Import Chrome's cookies on macOS, optionally filtered to hosts containing one
/// of `hosts`. Decryption failures are counted, not fatal.
pub fn import_chrome_macos(
    profile: Option<&str>,
    hosts: Option<&[String]>,
) -> Result<ImportSummary, String> {
    let key = derive_key(&keychain_password()?);
    let rows = read_rows(&cookies_db(profile)?, hosts)?;
    let total = rows.len() as u64;
    let mut cookies = Vec::new();
    let mut undecryptable = 0;
    for row in rows {
        match decrypt_value(&row.encrypted, &key) {
            Ok(value) => cookies.push(ImportedCookie {
                host: row.host,
                name: row.name,
                value,
                path: row.path,
                expires: chrome_time_to_unix(row.expires_utc),
                secure: row.secure,
                http_only: row.http_only,
            }),
            Err(_) => undecryptable += 1,
        }
    }
    Ok(ImportSummary {
        cookies,
        undecryptable,
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::{BlockEncryptMut, block_padding::Pkcs7};

    type Encryptor = cbc::Encryptor<Aes128>;

    /// Encrypt with Chrome's scheme and prepend `prefix`, as the browser stores it.
    fn seal(plaintext: &[u8], key: &[u8; 16], prefix: &[u8]) -> Vec<u8> {
        let ct = Encryptor::new(key.into(), &IV.into()).encrypt_padded_vec_mut::<Pkcs7>(plaintext);
        [prefix, &ct].concat()
    }

    #[test]
    fn v10_암호문을_왕복_복호화한다() {
        let key = derive_key(b"s3cr3t-keychain-pw");
        let blob = seal(b"session=abc123; theme=dark", &key, b"v10");
        assert_eq!(
            decrypt_value(&blob, &key).unwrap(),
            "session=abc123; theme=dark"
        );
        // v11 prefix decrypts the same way.
        let blob = seal(b"x=1", &key, b"v11");
        assert_eq!(decrypt_value(&blob, &key).unwrap(), "x=1");
    }

    #[test]
    fn 틀린_키는_실패하거나_다른_평문을_낸다() {
        let key = derive_key(b"right");
        let blob = seal(b"secret=42", &key, b"v10");
        // A wrong key fails PKCS7 unpadding or yields garbage — never the plaintext.
        let wrong = derive_key(b"wrong");
        assert_ne!(
            decrypt_value(&blob, &wrong).ok().as_deref(),
            Some("secret=42")
        );
    }

    #[test]
    fn 프리픽스가_없으면_평문으로_취급한다() {
        let key = derive_key(b"k");
        assert_eq!(decrypt_value(b"plainvalue", &key).unwrap(), "plainvalue");
    }

    #[test]
    fn derive_key는_결정적_16바이트() {
        let a = derive_key(b"pw");
        let b = derive_key(b"pw");
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert_ne!(derive_key(b"pw"), derive_key(b"other"));
    }

    #[test]
    fn chrome_시각을_unix초로_변환한다() {
        assert_eq!(chrome_time_to_unix(0), None);
        // 13_000_000_000_000_000 µs since 1601 ≈ 2011-11-26 in Unix seconds.
        let secs = chrome_time_to_unix(13_000_000_000_000_000).unwrap();
        assert!((secs - 1_355_526_400.0).abs() < 1.0, "got {secs}");
    }

    #[test]
    fn hex를_바이트로_디코드한다() {
        assert_eq!(
            decode_hex("76313061ff00"),
            vec![0x76, 0x31, 0x30, 0x61, 0xff, 0x00]
        );
        assert_eq!(decode_hex(""), Vec::<u8>::new());
    }
}
