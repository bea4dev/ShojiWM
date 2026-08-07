//! Tiny TOML-based i18n for the portal UI.
//!
//! Locale TOMLs are embedded with `include_str!` at compile time, parsed once
//! at startup via [`init`], and flattened into a `HashMap<String, String>`
//! keyed by dot-separated paths (`picker.heading` → "..."). [`t`] /
//! [`t_args`] look the active locale up first, falling back to `en_US` for
//! any key that's missing in the primary table (so partial translations
//! still work). If the loaded locale is unknown or none of the env hints
//! match, the portal defaults to `en_US`.
//!
//! Adding a locale = drop another `xx_YY.toml` next to this file with the
//! same set of keys, then extend [`detect_locale`] + the `match` in
//! [`init`].

use std::collections::HashMap;
use std::sync::OnceLock;

const EN_US: &str = include_str!("en_US.toml");
const JA_JP: &str = include_str!("ja_JP.toml");

static PRIMARY: OnceLock<HashMap<String, String>> = OnceLock::new();
static FALLBACK: OnceLock<HashMap<String, String>> = OnceLock::new();

/// Detect the active locale and load both it and the en_US fallback. Idempotent.
pub fn init() {
    if PRIMARY.get().is_some() {
        return;
    }
    let fallback = parse(EN_US);
    let _ = FALLBACK.set(fallback);

    let locale = detect_locale();
    let primary = match locale {
        "ja_JP" => parse(JA_JP),
        "en_US" => HashMap::new(),
        other => {
            tracing::info!(locale = other, "no translation table; using en_US");
            HashMap::new()
        }
    };
    tracing::info!(locale, "i18n initialized");
    let _ = PRIMARY.set(primary);
}

/// Look up the localized string for `key` (e.g. `picker.heading`). Returns
/// the key itself if neither the primary table nor the fallback has it.
pub fn t(key: &str) -> String {
    if let Some(p) = PRIMARY.get()
        && let Some(v) = p.get(key)
    {
        return v.clone();
    }
    if let Some(f) = FALLBACK.get()
        && let Some(v) = f.get(key)
    {
        return v.clone();
    }
    key.to_string()
}

/// Look up `key` then substitute `{name}` placeholders with values from `args`.
pub fn t_args(key: &str, args: &[(&str, &str)]) -> String {
    let mut s = t(key);
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

fn detect_locale() -> &'static str {
    let raw = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    // POSIX locale strings look like `ja_JP.UTF-8@modifier`; strip codeset
    // and modifier suffixes before matching.
    let base = raw
        .split(['.', '@'])
        .next()
        .unwrap_or("")
        .trim();
    match base {
        "ja_JP" | "ja" => "ja_JP",
        "en_US" | "en" | "C" | "POSIX" | "" => "en_US",
        _ => "en_US",
    }
}

fn parse(src: &str) -> HashMap<String, String> {
    let value: toml::Value = match src.parse() {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("i18n: failed to parse TOML: {e}");
            return HashMap::new();
        }
    };
    let mut out = HashMap::new();
    flatten(&value, String::new(), &mut out);
    out
}

fn flatten(value: &toml::Value, prefix: String, out: &mut HashMap<String, String>) {
    match value {
        toml::Value::Table(t) => {
            for (k, v) in t {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(v, key, out);
            }
        }
        toml::Value::String(s) => {
            out.insert(prefix, s.clone());
        }
        _ => {
            // Other TOML scalar types are unexpected in a translation table;
            // ignore them silently. Adding a key with a non-string value
            // would surface in the failing parse path during development.
        }
    }
}
