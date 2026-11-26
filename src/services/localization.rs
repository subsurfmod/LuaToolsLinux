//! Localization Service (LT-06)
//!
//! Provides 30-language support by loading translation JSON files from assets/translations/
//! and reading AppSettings.language with Steam UI language auto-detection fallback.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::config::AppSettings;
use crate::services::steam_service::SteamService;

static LOCALIZATION_SERVICE: OnceLock<Arc<RwLock<Option<LocalizationService>>>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslationFile {
    pub lang_code: String,
    pub lang_name: String,
    pub native_name: String,
    pub translations: HashMap<String, String>,
}

#[derive(Clone)]
pub struct LocalizationService {
    current_lang: String,
    translations: HashMap<String, String>,
    available_languages: Vec<LanguageInfo>,
    fallback_lang: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageInfo {
    pub code: String,
    pub name: String,
    pub native_name: String,
}

impl LocalizationService {
    /// Gets or creates the global localization service.
    pub fn global() -> Arc<RwLock<Option<LocalizationService>>> {
        LOCALIZATION_SERVICE
            .get_or_init(|| Arc::new(RwLock::new(None)))
            .clone()
    }

    /// Initializes the global localization service with the given settings.
    pub fn init_global(settings: &AppSettings) -> Result<(), String> {
        let global = Self::global();
        let mut guard = global.write().unwrap();

        let service = Self::new(&settings.language)?;
        *guard = Some(service);
        Ok(())
    }

    /// Creates a new LocalizationService, loading translations for the given language code.
    /// Falls back to English if the language is not available.
    fn new(lang_code: &str) -> Result<Self, String> {
        let assets_dir = Self::assets_translations_dir();
        let mut available = Vec::new();
        let mut translations = HashMap::new();
        let fallback_lang = "en".to_string();

        // Scan for available translation files
        if let Ok(entries) = fs::read_dir(&assets_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "json") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        if let Ok(content) = fs::read_to_string(&path) {
                            if let Ok(json) = serde_json::from_str::<Value>(&content) {
                                let name = json.get("app.name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(stem)
                                    .to_string();
                                let native_name = json.get("label.detected_language")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(name.as_str())
                                    .to_string();

                                available.push(LanguageInfo {
                                    code: stem.to_string(),
                                    name: name.clone(),
                                    native_name,
                                });

                                // Load translations for this language
                                if stem == lang_code || (lang_code != "en" && stem == "en") {
                                    if let Some(obj) = json.as_object() {
                                        for (k, v) in obj {
                                            if let Some(s) = v.as_str() {
                                                if translations.get(k).is_none() {
                                                    translations.insert(k.clone(), s.to_string());
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // If requested language wasn't loaded (not found), load English as fallback
        if !translations.contains_key("app.name") && lang_code != "en" {
            let en_path = assets_dir.join("en.json");
            if let Ok(content) = fs::read_to_string(&en_path) {
                if let Ok(json) = serde_json::from_str::<Value>(&content) {
                    if let Some(obj) = json.as_object() {
                        for (k, v) in obj {
                            if let Some(s) = v.as_str() {
                                translations.insert(k.clone(), s.to_string());
                            }
                        }
                    }
                }
            }
        }

        // Also ensure we have English as ultimate fallback
        if !translations.contains_key("app.name") {
            let en_path = assets_dir.join("en.json");
            if let Ok(content) = fs::read_to_string(&en_path) {
                if let Ok(json) = serde_json::from_str::<Value>(&content) {
                    if let Some(obj) = json.as_object() {
                        for (k, v) in obj {
                            if let Some(s) = v.as_str() {
                                translations.insert(k.clone(), s.to_string());
                            }
                        }
                    }
                }
            }
        }

        available.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(Self {
            current_lang: if available.iter().any(|l| l.code == lang_code) { lang_code.to_string() } else { fallback_lang.clone() },
            translations,
            available_languages: available,
            fallback_lang,
        })
    }

    fn assets_translations_dir() -> PathBuf {
        // Try multiple locations for the translations directory
        let candidates = vec![
            // From Cargo manifest directory (when running from source)
            std::env::var("CARGO_MANIFEST_DIR")
                .ok()
                .map(|p| PathBuf::from(p).join("src/assets/translations")),
            // From current executable directory
            std::env::current_exe()
                .ok()
                .map(|p| p.parent().unwrap().join("assets/translations")),
            // From current working directory
            Some(PathBuf::from("src/assets/translations")),
            // From installed location
            dirs::home_dir().map(|h| h.join(".local/share/luatools/translations")),
        ];

        for candidate in candidates.into_iter().flatten() {
            if candidate.exists() {
                return candidate;
            }
        }

        // Default to source location
        PathBuf::from("src/assets/translations")
    }

    /// Returns the current language code.
    pub fn current_lang(&self) -> &str {
        &self.current_lang
    }

    /// Returns available languages.
    pub fn available_languages(&self) -> &[LanguageInfo] {
        &self.available_languages
    }

    /// Sets the current language and reloads translations.
    pub fn set_language(&mut self, lang_code: &str) -> Result<(), String> {
        if !self.available_languages.iter().any(|l| l.code == lang_code) {
            return Err(format!("Language '{}' not available", lang_code));
        }

        let assets_dir = Self::assets_translations_dir();
        let lang_path = assets_dir.join(format!("{}.json", lang_code));

        let mut new_translations = HashMap::new();

        // Load requested language
        if let Ok(content) = fs::read_to_string(&lang_path) {
            if let Ok(json) = serde_json::from_str::<Value>(&content) {
                if let Some(obj) = json.as_object() {
                    for (k, v) in obj {
                        if let Some(s) = v.as_str() {
                            new_translations.insert(k.clone(), s.to_string());
                        }
                    }
                }
            }
        }

        // Load English as fallback for any missing keys
        let en_path = assets_dir.join("en.json");
        if let Ok(content) = fs::read_to_string(&en_path) {
            if let Ok(json) = serde_json::from_str::<Value>(&content) {
                if let Some(obj) = json.as_object() {
                    for (k, v) in obj {
                        if let Some(s) = v.as_str() {
                            new_translations.entry(k.clone()).or_insert(s.to_string());
                        }
                    }
                }
            }
        }

        self.current_lang = lang_code.to_string();
        self.translations = new_translations;
        Ok(())
    }

    /// Gets a translated string by key, with optional format arguments.
    pub fn t(&self, key: &str, args: &[(&str, &str)]) -> String {
        let mut result = self.translations.get(key)
            .cloned()
            .unwrap_or_else(|| {
                // Return key as fallback if not found
                key.to_string()
            });

        // Replace format placeholders {key}
        for (k, v) in args {
            let placeholder = format!("{{{}}}", k);
            result = result.replace(&placeholder, v);
        }

        result
    }

    /// Gets a translated string by key (no format args).
    pub fn tr(&self, key: &str) -> String {
        self.t(key, &[])
    }

    /// Detects the Steam UI language and returns a supported language code.
    /// Falls back to "en" if detection fails or language is not supported.
    pub fn detect_steam_ui_language() -> String {
        SteamService::detect_steam_ui_language_code(None)
            .and_then(|code| Self::map_steam_language_to_supported(&code))
            .unwrap_or_else(|| "en".to_string())
    }

    /// Maps Steam's language code to our supported language codes.
    fn map_steam_language_to_supported(steam_lang: &str) -> Option<String> {
        let steam_lang = steam_lang.to_lowercase();
        match steam_lang.as_str() {
            "english" | "en" => Some("en".to_string()),
            "schinese" | "zh-cn" | "zh" => Some("zh".to_string()),
            "tchinese" | "zh-tw" => Some("zh".to_string()), // Could add zh-TW later
            "russian" | "ru" => Some("ru".to_string()),
            "german" | "de" => Some("de".to_string()),
            "french" | "fr" => Some("fr".to_string()),
            "spanish" | "es" => Some("es".to_string()),
            "brazilian" | "pt-br" => Some("pt".to_string()),
            "portuguese" | "pt" => Some("pt".to_string()),
            "polish" | "pl" => Some("pl".to_string()),
            "italian" | "it" => Some("it".to_string()),
            "korean" | "ko" => Some("ko".to_string()),
            "japanese" | "ja" => Some("ja".to_string()),
            "turkish" | "tr" => Some("tr".to_string()),
            "czech" | "cs" => Some("cs".to_string()),
            "romanian" | "ro" => Some("ro".to_string()),
            "hungarian" | "hu" => Some("hu".to_string()),
            "danish" | "da" => Some("da".to_string()),
            "dutch" | "nl" => Some("nl".to_string()),
            "norwegian" | "no" => Some("no".to_string()),
            "swedish" | "sv" => Some("sv".to_string()),
            "finnish" | "fi" => Some("fi".to_string()),
            "thai" | "th" => Some("th".to_string()),
            "vietnamese" | "vi" => Some("vi".to_string()),
            "bulgarian" | "bg" => Some("bg".to_string()),
            "ukrainian" | "uk" => Some("uk".to_string()),
            "greek" | "el" => Some("el".to_string()),
            "arabic" | "ar" => Some("ar".to_string()),
            _ => None,
        }
    }

    /// Returns the translation service, initializing if needed.
    pub fn get_service() -> Result<LocalizationService, String> {
        let global = Self::global();
        let guard = global.read().unwrap();
        guard.as_ref().cloned().ok_or_else(|| {
            "Localization service not initialized".into()
        })
    }
}

/// Convenience function to get a translated string (uses global service if initialized).
pub fn tr(key: &str) -> String {
    LocalizationService::get_service()
        .map(|s| s.tr(key))
        .unwrap_or_else(|_| key.to_string())
}

/// Convenience function to get a translated string with format args.
pub fn trf(key: &str, args: &[(&str, &str)]) -> String {
    LocalizationService::get_service()
        .map(|s| s.t(key, args))
        .unwrap_or_else(|_| {
            let mut result = key.to_string();
            for (k, v) in args {
                result = result.replace(&format!("{{{}}}", k), v);
            }
            result
        })
}

/// Returns the current language code from global service.
pub fn current_lang() -> String {
    LocalizationService::get_service()
        .map(|s| s.current_lang().to_string())
        .unwrap_or_else(|_| "en".to_string())
}

/// Returns available languages from global service.
pub fn available_languages() -> Vec<LanguageInfo> {
    LocalizationService::get_service()
        .map(|s| s.available_languages().to_vec())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::env;

    #[test]
    fn test_load_english_translations() {
        let dir = tempdir().unwrap();
        let trans_dir = dir.path().join("src/assets/translations");
        fs::create_dir_all(&trans_dir).unwrap();

        let en_content = r#"{
            "app.name": "Test App",
            "menu.file": "File",
            "label.game": "Game: {name}"
        }"#;
        fs::write(trans_dir.join("en.json"), en_content).unwrap();

        // Override assets dir for test
        env::set_var("CARGO_MANIFEST_DIR", dir.path().to_str().unwrap());

        let service = LocalizationService::new("en").unwrap();
        assert_eq!(service.tr("app.name"), "Test App");
        assert_eq!(service.tr("menu.file"), "File");
        assert_eq!(service.t("label.game", &[("name", "TestGame")]), "Game: TestGame");
    }

    #[test]
    fn test_fallback_to_english() {
        let dir = tempdir().unwrap();
        let trans_dir = dir.path().join("src/assets/translations");
        fs::create_dir_all(&trans_dir).unwrap();

        let en_content = r#"{"app.name": "Test App", "menu.file": "File"}"#;
        fs::write(trans_dir.join("en.json"), en_content).unwrap();

        // Create a partial zh.json without all keys
        let zh_content = r#"{"app.name": "测试应用"}"#;
        fs::write(trans_dir.join("zh.json"), zh_content).unwrap();

        env::set_var("CARGO_MANIFEST_DIR", dir.path().to_str().unwrap());

        let service = LocalizationService::new("zh").unwrap();
        assert_eq!(service.tr("app.name"), "测试应用");
        // Should fall back to English for missing key
        assert_eq!(service.tr("menu.file"), "File");
    }

    #[test]
    fn test_language_detection_mapping() {
        assert_eq!(LocalizationService::map_steam_language_to_supported("english"), Some("en".to_string()));
        assert_eq!(LocalizationService::map_steam_language_to_supported("schinese"), Some("zh".to_string()));
        assert_eq!(LocalizationService::map_steam_language_to_supported("russian"), Some("ru".to_string()));
        assert_eq!(LocalizationService::map_steam_language_to_supported("german"), Some("de".to_string()));
        assert_eq!(LocalizationService::map_steam_language_to_supported("unknown"), None);
    }
}