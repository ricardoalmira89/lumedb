//! Passwords and API keys in the operating system's keyring (Secret Service on Linux, Keychain on
//! macOS, Credential Manager on Windows) instead of the JSON files.
//!
//! Every secret is an entry of the service `navidog`, named after what it belongs to, e.g.
//! `<connection id>/password` or `ai/anthropic`. When no keyring is available (minimal Linux
//! setups, or `NAVIDOG_NO_KEYRING=1`), callers keep the secret in their file instead, which is then
//! only readable by the user.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use keyring::{Entry, Error};

const SERVICE: &str = "navidog";

/// Values known to be in the keyring, so that saving unchanged settings does not rewrite them.
fn known() -> &'static Mutex<HashMap<String, String>> {
    static KNOWN: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    KNOWN.get_or_init(Default::default)
}

/// Whether a keyring can be used (checked once).
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        if std::env::var_os("NAVIDOG_NO_KEYRING").is_some_and(|v| !v.is_empty() && v != "0") {
            return false;
        }
        match Entry::new(SERVICE, "probe").and_then(|e| e.get_password()) {
            Ok(_) | Err(Error::NoEntry) => true,
            Err(_) => false,
        }
    })
}

/// Reads a secret; `None` when it is not stored or there is no keyring.
pub fn get(account: &str) -> Option<String> {
    if !available() {
        return None;
    }
    let value = Entry::new(SERVICE, account).ok()?.get_password().ok()?;
    known().lock().unwrap().insert(account.to_string(), value.clone());
    Some(value)
}

/// Stores a secret; an empty value deletes it.
pub fn set(account: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        delete(account);
        return Ok(());
    }
    if known().lock().unwrap().get(account).is_some_and(|v| v == value) {
        return Ok(());
    }
    Entry::new(SERVICE, account)?.set_password(value)?;
    known().lock().unwrap().insert(account.to_string(), value.to_string());
    Ok(())
}

pub fn delete(account: &str) {
    known().lock().unwrap().remove(account);
    if available() {
        if let Ok(e) = Entry::new(SERVICE, account) {
            let _ = e.delete_credential();
        }
    }
}

/// Deletes the stored secrets whose name starts with `prefix` and that `keep` rejects (e.g. those
/// of connections that no longer exist). Only entries read or written in this session are known.
pub fn prune(prefix: &str, keep: impl Fn(&str) -> bool) {
    let stale: Vec<String> = known().lock().unwrap().keys().filter(|a| a.starts_with(prefix) && !keep(a)).cloned().collect();
    for a in stale {
        delete(&a);
    }
}
