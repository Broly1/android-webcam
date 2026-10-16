//! Interface translations.
//!
//! Each language is a flat JSON file in `locales/` mapping message keys to text, embedded into the
//! binary at compile time. `en.json` is the reference: every key must exist there, and any key
//! missing from another language falls back to English. Placeholders are written `{name}`.
//!
//! To add a language, copy `locales/en.json` to `locales/<code>.json` (e.g. `fr`, `de`, `pt_BR`),
//! translate the values, and register it in `LOCALES` below.

use std::collections::HashMap;
use std::env;
use std::sync::OnceLock;

/// Locale code (as in `LANG`, without encoding) and its embedded message file.
const LOCALES: &[(&str, &str)] = &[
    ("en", include_str!("../locales/en.json")),
    ("es", include_str!("../locales/es.json")),
    ("pt_BR", include_str!("../locales/pt_BR.json")),
];

const FALLBACK: &str = "en";

struct Catalog {
    active: HashMap<String, String>,
    fallback: HashMap<String, String>,
}

static CATALOG: OnceLock<Catalog> = OnceLock::new();

fn parse(code: &str) -> HashMap<String, String> {
    let data = LOCALES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, d)| *d)
        .unwrap_or("{}");
    serde_json::from_str(data).unwrap_or_else(|e| panic!("locales/{}.json is invalid: {}", code, e))
}

fn catalog() -> &'static Catalog {
    CATALOG.get_or_init(|| Catalog {
        active: parse(&detect_locale()),
        fallback: parse(FALLBACK),
    })
}

/// Picks the best embedded locale from the environment, following gettext's precedence:
/// `ANDROID_WEBCAM_LANG` (override), `LANGUAGE` (colon-separated list), `LC_ALL`,
/// `LC_MESSAGES`, then `LANG`.
fn detect_locale() -> String {
    let mut candidates = Vec::new();
    if let Ok(v) = env::var("ANDROID_WEBCAM_LANG") {
        candidates.push(v);
    }
    // LANGUAGE is ignored by gettext when the locale is "C", so only use it alongside a real one.
    let base = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| env::var(k).ok())
        .find(|v| !v.is_empty());
    if let Some(ref b) = base {
        if b != "C" && b != "POSIX" {
            if let Ok(list) = env::var("LANGUAGE") {
                candidates.extend(list.split(':').map(str::to_string));
            }
        }
        candidates.push(b.clone());
    }

    let available: Vec<&str> = LOCALES.iter().map(|(c, _)| *c).collect();
    candidates
        .iter()
        .find_map(|c| match_locale(c, &available))
        .unwrap_or_else(|| FALLBACK.to_string())
}

/// Matches a POSIX locale like `pt_BR.UTF-8@euro` against the available codes: exact
/// `pt_BR` first, then the bare language `pt`, then any regional variant of it (`pt_PT` -> `pt_BR`).
pub fn match_locale(raw: &str, available: &[&str]) -> Option<String> {
    let name = raw.split(['.', '@']).next()?.replace('-', "_");
    if name.is_empty() || name == "C" || name == "POSIX" {
        return None;
    }
    let lang = name.split('_').next()?;
    let prefix = format!("{}_", lang);
    [&name, lang]
        .iter()
        .find_map(|want| available.iter().find(|c| c.eq_ignore_ascii_case(want)))
        .or_else(|| available.iter().find(|c| c.starts_with(&prefix)))
        .map(|c| c.to_string())
}

/// Translated text for `key`, falling back to English, then to the key itself.
pub fn tr(key: &str) -> &'static str {
    let cat = catalog();
    cat.active
        .get(key)
        .or_else(|| cat.fallback.get(key))
        .map(String::as_str)
        .unwrap_or_else(|| {
            debug_assert!(false, "missing translation key: {}", key);
            ""
        })
}

/// Translated text for `key` with `{name}` placeholders replaced.
pub fn trf(key: &str, args: &[(&str, &str)]) -> String {
    fill(tr(key), args)
}

/// Replaces `{name}` placeholders in `template`.
pub fn fill(template: &str, args: &[(&str, &str)]) -> String {
    args.iter().fold(template.to_string(), |s, (name, value)| {
        s.replace(&format!("{{{}}}", name), value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn placeholders(s: &str) -> BTreeSet<String> {
        s.split('{')
            .skip(1)
            .filter_map(|p| p.split_once('}').map(|(name, _)| name.to_string()))
            .collect()
    }

    #[test]
    fn test_all_locales_match_english() {
        let en = parse(FALLBACK);
        for (code, _) in LOCALES {
            let other = parse(code);
            for key in other.keys() {
                assert!(en.contains_key(key), "{}.json has unknown key {}", code, key);
            }
            for (key, text) in &en {
                let Some(t) = other.get(key) else {
                    panic!("{}.json is missing key {}", code, key);
                };
                assert_eq!(
                    placeholders(text),
                    placeholders(t),
                    "{}.json key {} has different placeholders",
                    code,
                    key
                );
            }
        }
    }

    #[test]
    fn test_match_locale() {
        let avail = ["en", "es", "pt_BR"];
        assert_eq!(match_locale("pt_BR.UTF-8", &avail).as_deref(), Some("pt_BR"));
        assert_eq!(match_locale("pt_PT.UTF-8", &avail).as_deref(), Some("pt_BR"));
        assert_eq!(match_locale("pt", &avail).as_deref(), Some("pt_BR"));
        assert_eq!(match_locale("es_MX.UTF-8", &avail).as_deref(), Some("es"));
        assert_eq!(match_locale("es-AR", &avail).as_deref(), Some("es"));
        assert_eq!(match_locale("en_US.UTF-8", &avail).as_deref(), Some("en"));
        assert_eq!(match_locale("de_DE.UTF-8", &avail), None);
        assert_eq!(match_locale("C.UTF-8", &avail), None);
        assert_eq!(match_locale("", &avail), None);
    }

    #[test]
    fn test_fill() {
        assert_eq!(
            fill("{res} crashed, relaunched at {fallback}", &[("res", "4K"), ("fallback", "1080p")]),
            "4K crashed, relaunched at 1080p"
        );
    }
}
