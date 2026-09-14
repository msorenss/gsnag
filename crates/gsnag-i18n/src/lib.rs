//! Shared UTF-8 message catalogs. English messages are stable translation keys.
use anyhow::{Result, ensure};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{OnceLock, RwLock},
};

pub const LANGUAGES: &[(&str, &str)] = &[("en", "English"), ("sv", "Svenska"), ("de", "Deutsch")];
static LANGUAGE: OnceLock<RwLock<String>> = OnceLock::new();
static CATALOGS: OnceLock<HashMap<&'static str, HashMap<String, String>>> = OnceLock::new();

pub fn normalize(value: &str) -> Option<&'static str> {
    let code = value.split(['_', '-', '.', '@', ':']).next().unwrap_or("");
    if code == "C" || code == "POSIX" {
        return Some("en");
    }
    LANGUAGES
        .iter()
        .find(|(id, _)| code.eq_ignore_ascii_case(id))
        .map(|(id, _)| *id)
}
fn preference() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))?;
    dir.is_absolute().then(|| dir.join("gsnag/language"))
}
fn detect() -> String {
    if let Ok(value) = std::env::var("GSNAG_LANGUAGE")
        && let Some(code) = normalize(&value)
    {
        return code.into();
    }
    if let Some(path) = preference()
        && let Ok(value) = std::fs::read_to_string(path)
        && let Some(code) = normalize(value.trim())
    {
        return code.into();
    }
    for variable in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(variable)
            && !value.is_empty()
        {
            return normalize(&value).unwrap_or("en").into();
        }
    }
    "en".into()
}
pub fn language() -> String {
    LANGUAGE
        .get_or_init(|| RwLock::new(detect()))
        .read()
        .unwrap()
        .clone()
}
pub fn set_language(code: &str) -> Result<()> {
    let code = normalize(code).ok_or_else(|| anyhow::anyhow!("Unsupported language: {code}"))?;
    *LANGUAGE
        .get_or_init(|| RwLock::new(detect()))
        .write()
        .unwrap() = code.into();
    Ok(())
}
pub fn save_language(code: &str) -> Result<()> {
    ensure!(
        LANGUAGES.iter().any(|(id, _)| *id == code) || code == "auto",
        "Unsupported language: {code}"
    );
    let path = preference().ok_or_else(|| anyhow::anyhow!("No user configuration directory"))?;
    if code == "auto" {
        match std::fs::remove_file(&path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
        return set_language(&detect());
    }
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    use std::io::Write;
    writeln!(temp, "{code}")?;
    temp.as_file().sync_all()?;
    temp.persist(&path).map_err(|e| e.error)?;
    set_language(code)
}
pub fn translate(language: &str, message: &str) -> String {
    let catalogs = CATALOGS.get_or_init(|| {
        HashMap::from([
            (
                "sv",
                serde_json::from_str(include_str!("../../../locales/sv.json"))
                    .expect("validated Swedish catalog"),
            ),
            (
                "de",
                serde_json::from_str(include_str!("../../../locales/de.json"))
                    .expect("validated German catalog"),
            ),
        ])
    });
    catalogs
        .get(language)
        .and_then(|c| c.get(message))
        .filter(|s| !s.is_empty())
        .map(String::as_str)
        .unwrap_or(message)
        .to_owned()
}
pub fn tr(message: &str) -> String {
    translate(&language(), message)
}
pub fn format(message: &str, args: &[(&str, String)]) -> String {
    let mut text = tr(message);
    for (key, value) in args {
        text = text.replace(&format!("{{{key}}}"), value);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn locale_normalization_and_fallback() {
        assert_eq!(normalize("sv_SE.UTF-8"), Some("sv"));
        assert_eq!(normalize("de-DE"), Some("de"));
        assert_eq!(normalize("C.UTF-8"), Some("en"));
        assert_eq!(normalize("fr_FR"), None);
        assert_eq!(translate("xx", "Save"), "Save");
        assert_eq!(translate("sv", "Save"), "Spara");
    }
    #[test]
    fn catalogs_have_identical_keys_and_preserve_placeholders() {
        let sv: HashMap<String, String> =
            serde_json::from_str(include_str!("../../../locales/sv.json")).unwrap();
        let de: HashMap<String, String> =
            serde_json::from_str(include_str!("../../../locales/de.json")).unwrap();
        assert_eq!(sv.len(), de.len());
        for (key, value) in &sv {
            assert!(de.contains_key(key), "{key}");
            for token in key
                .split('{')
                .skip(1)
                .filter_map(|s| s.split_once('}').map(|(s, _)| format!("{{{s}}}")))
            {
                assert!(
                    value.contains(&token) && de[key].contains(&token),
                    "{key}: {token}"
                );
            }
        }
    }
}
