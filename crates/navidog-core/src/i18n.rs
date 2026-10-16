//! Interface translations.
//!
//! English is the source language: every user-visible string is written in English and wrapped in
//! [`tr`] (or [`trf!`](crate::trf) when it has placeholders). Other languages are JSON catalogs that
//! map the English text to its translation; a missing entry falls back to English.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::config::config_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Lang {
    #[default]
    En,
    Es,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::En, Lang::Es];

    /// Name of the language in that language, for the language menu.
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Es => "Español",
        }
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn lang() -> Lang {
    match LANG.load(Ordering::Relaxed) {
        1 => Lang::Es,
        _ => Lang::En,
    }
}

pub fn set_lang(l: Lang) {
    LANG.store(l as u8, Ordering::Relaxed);
}

fn catalog(l: Lang) -> Option<&'static HashMap<String, String>> {
    static ES: OnceLock<HashMap<String, String>> = OnceLock::new();
    match l {
        Lang::En => None,
        Lang::Es => Some(ES.get_or_init(|| serde_json::from_str(include_str!("../i18n/es.json")).expect("i18n/es.json is not valid JSON"))),
    }
}

/// Translates an English interface string into the current language.
pub fn tr(s: &'static str) -> &'static str {
    match catalog(lang()).and_then(|c| c.get(s)) {
        Some(t) => t.as_str(),
        None => s,
    }
}

/// Like [`tr`] for words whose translation depends on where they appear ("View" as a menu is "Ver",
/// as an object "Vista"). The catalog key is `"<context>|<text>"`; without it, it falls back to `tr(s)`.
pub fn trc(context: &'static str, s: &'static str) -> &'static str {
    match catalog(lang()).and_then(|c| c.get(&format!("{context}|{s}"))) {
        Some(t) => t.as_str(),
        None => tr(s),
    }
}

/// Translates a string with named `{placeholders}`: `trf!("Delete {n} rows?", n = count)`.
#[macro_export]
macro_rules! trf {
    ($s:literal $(, $k:ident = $v:expr)* $(,)?) => {{
        #[allow(unused_mut)]
        let mut out = $crate::i18n::tr($s).to_string();
        $( out = out.replace(concat!("{", stringify!($k), "}"), &($v).to_string()); )*
        out
    }};
}

// ---------------------------------------------------------------- Preferences

/// Interface preferences kept between sessions (`~/.config/navidog/prefs.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub lang: Lang,
    pub dark: bool,
    /// Records per page when browsing a table (Navicat's default is 50).
    pub page_size: usize,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { lang: Lang::default(), dark: false, page_size: 50 }
    }
}

pub fn load_prefs() -> Prefs {
    config_dir()
        .ok()
        .and_then(|d| std::fs::read_to_string(d.join("prefs.json")).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_prefs(p: &Prefs) -> anyhow::Result<()> {
    let d = config_dir()?;
    std::fs::create_dir_all(&d)?;
    std::fs::write(d.join("prefs.json"), serde_json::to_string_pretty(p)?)?;
    Ok(())
}
