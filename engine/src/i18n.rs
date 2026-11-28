//! English (default) and Dutch strings for user-facing API text.

use std::collections::HashMap;
use std::sync::OnceLock;

static EN: &str = include_str!("../i18n/en.json");
static NL: &str = include_str!("../i18n/nl.json");

fn tables() -> &'static HashMap<&'static str, HashMap<String, String>> {
    static T: OnceLock<HashMap<&'static str, HashMap<String, String>>> = OnceLock::new();
    T.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert("en", serde_json::from_str(EN).expect("en.json"));
        m.insert("nl", serde_json::from_str(NL).expect("nl.json"));
        m
    })
}

pub fn languages() -> &'static [&'static str] {
    &["en", "nl"]
}

/// Pick a language from an Accept-Language header or a `lang` value.
pub fn pick(lang: Option<&str>) -> &'static str {
    let l = lang.unwrap_or("").to_ascii_lowercase();
    for part in l.split(',') {
        let code = part.trim().split(['-', ';']).next().unwrap_or("");
        if code == "nl" {
            return "nl";
        }
        if code == "en" {
            return "en";
        }
    }
    "en"
}

pub fn table(lang: &str) -> &'static HashMap<String, String> {
    tables().get(pick(Some(lang))).unwrap()
}

pub fn t(lang: &str, key: &str) -> String {
    table(lang)
        .get(key)
        .or_else(|| tables()["en"].get(key))
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

pub fn tf(lang: &str, key: &str, detail: &str) -> String {
    t(lang, key).replace("{detail}", detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_languages_have_the_same_keys() {
        let en = &tables()["en"];
        let nl = &tables()["nl"];
        for k in en.keys() {
            assert!(nl.contains_key(k), "nl.json misses {k}");
        }
        for k in nl.keys() {
            assert!(en.contains_key(k), "en.json misses {k}");
        }
        assert_eq!(pick(Some("nl-NL,nl;q=0.9,en;q=0.8")), "nl");
        assert_eq!(pick(None), "en");
    }
}
