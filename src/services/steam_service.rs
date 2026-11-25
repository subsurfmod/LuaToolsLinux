//! Steam discovery & multi-library game scanner.

use std::fs;
use std::path::{Path, PathBuf};

use crate::models::steam::{
    default_steam_candidates, discover_libraries, get_game_name_from_acf, get_install_dir_from_acf,
    scan_installed_games, SteamInstallation, SteamLibrary,
};

pub fn detect_steam_install(override_path: Option<&str>) -> Option<SteamInstallation> {
    let mut candidates = Vec::new();
    if let Some(ov) = override_path.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(expanded) = expand_tilde(ov) {
            candidates.push(expanded);
        }
    }
    candidates.extend(default_steam_candidates());

    for cand in candidates {
        if !cand.exists() {
            continue;
        }
        let root = fs::canonicalize(&cand).unwrap_or(cand);
        let has_markers = root.join("steam.sh").exists()
            || root.join("steamapps").exists()
            || root.join("config").exists();
        if has_markers {
            let libraries = discover_libraries(&root);
            return Some(SteamInstallation {
                config_path: root.join("config"),
                stplug_in_path: root.join("config").join("stplug-in"),
                root,
                libraries,
            });
        }
    }
    None
}

pub fn expand_tilde(path_str: &str) -> Option<PathBuf> {
    if let Some(rest) = path_str.strip_prefix("~/") {
        return Some(dirs::home_dir()?.join(rest));
    }
    if path_str == "~" {
        return dirs::home_dir();
    }
    Some(PathBuf::from(path_str))
}

#[derive(Debug, Clone, Default)]
pub struct SteamService {
    pub installation: Option<SteamInstallation>,
}

impl SteamService {
    pub fn new() -> Self {
        Self { installation: None }
    }

    pub fn detect(&mut self, override_path: Option<&str>) -> Option<SteamInstallation> {
        self.installation = detect_steam_install(override_path);
        self.installation.clone()
    }

    pub fn is_available(&self) -> bool {
        self.installation
            .as_ref()
            .map(SteamInstallation::is_valid)
            .unwrap_or(false)
    }

    pub fn libraries(&self) -> &[SteamLibrary] {
        self.installation
            .as_ref()
            .map(|i| i.libraries.as_slice())
            .unwrap_or(&[])
    }

    pub fn scan_all_games(&self) -> Vec<u32> {
        let mut all = Vec::new();
        for lib in self.libraries() {
            all.extend(scan_installed_games(lib));
        }
        all.sort_unstable();
        all.dedup();
        all
    }

    pub fn get_game_name(&self, app_id: u32) -> Option<String> {
        for lib in self.libraries() {
            if let Some(name) = get_game_name_from_acf(lib, app_id) {
                return Some(name);
            }
        }
        None
    }

    pub fn find_install_dir(&self, app_id: u32) -> Option<PathBuf> {
        for lib in self.libraries() {
            if let Some(dir) = get_install_dir_from_acf(lib, app_id) {
                if dir.exists() {
                    return Some(dir);
                }
            }
        }
        None
    }

    pub fn list_lua_app_ids(stplug_dir: &Path) -> Vec<u32> {
        let mut ids = Vec::new();
        if let Ok(entries) = fs::read_dir(stplug_dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("lua") {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        if let Ok(aid) = stem.parse::<u32>() {
                            ids.push(aid);
                        }
                    }
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Terminates all running Steam client, webhelper, overlay, and Steam Runtime processes cleanly.
    /// Sends `SIGTERM` first, waits up to 2 seconds for clean exit, then sends `SIGKILL` to any
    /// stubborn processes.
    pub fn terminate_steam_instances() -> Result<usize, String> {
        crate::steam_bridge::preload_loader::kill_all_steam_instances()
    }

    /// Checks whether any Steam client or helper process is currently running.
    pub fn is_steam_running() -> bool {
        crate::steam_bridge::preload_loader::find_steam_pid().is_some()
    }

    /// Cleanly terminates any running Steam instances and relaunches Steam via `steam-sls`
    /// (with `LD_AUDIT=library-inject.so:SLSsteam.so`) so SLSsteam is retained without 32-bit / 64-bit LD_PRELOAD crashes.
    pub fn relaunch_steam_sls(override_root: Option<&Path>) -> Result<(), String> {
        // 1. Terminate all running Steam processes
        let _ = Self::terminate_steam_instances();

        // 2. Brief grace delay for socket unbinding
        std::thread::sleep(std::time::Duration::from_millis(150));

        // 3. Resolve steam-sls launcher path
        let home = dirs::home_dir().ok_or_else(|| "Cannot determine HOME directory".to_string())?;
        let steam_sls_bin = home.join(".local/bin/steam-sls");

        let mut cmd = if steam_sls_bin.is_file() {
            std::process::Command::new(&steam_sls_bin)
        } else {
            // Find steam binary (override_root/steam.sh, native ~/.local/share/Steam/steam.sh,
            // Flatpak ~/.var/app/com.valvesoftware.Steam/.local/share/Steam/steam.sh, or fallback to PATH)
            let native_steam_sh = home.join(".local/share/Steam/steam.sh");
            let flatpak_steam_sh =
                home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/steam.sh");

            let steam_bin = override_root
                .map(|r| r.join("steam.sh"))
                .filter(|p| p.is_file())
                .or_else(|| native_steam_sh.is_file().then_some(native_steam_sh))
                .or_else(|| flatpak_steam_sh.is_file().then_some(flatpak_steam_sh))
                .unwrap_or_else(|| PathBuf::from("steam"));
            let mut c = std::process::Command::new(steam_bin);

            // Retain library-inject.so + SLSsteam.so via LD_AUDIT
            let sls_dir = home.join(".local/share/SLSsteam");
            let lib_inject_so = sls_dir.join("library-inject.so");
            let sls_so = sls_dir.join("SLSsteam.so");
            let mut audit_entries = Vec::new();
            if lib_inject_so.is_file() {
                audit_entries.push(lib_inject_so.to_string_lossy().into_owned());
            }
            if sls_so.is_file() {
                audit_entries.push(sls_so.to_string_lossy().into_owned());
            }
            if !audit_entries.is_empty() {
                c.env("LD_AUDIT", audit_entries.join(":"));
            }
            c
        };

        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to spawn steam-sls: {}", e))?;

        Ok(())
    }

    /// Instance method: cleanly terminates running Steam and relaunches via `steam-sls`.
    pub fn relaunch_via_sls(&self, override_root: Option<&Path>) -> Result<(), String> {
        let root = override_root.or_else(|| self.installation.as_ref().map(|i| i.root.as_path()));
        Self::relaunch_steam_sls(root)
    }

    /// Extracts Steam's UI language string from VDF text (`config.vdf` or `registry.vdf`).
    /// Prefers `"Language"` inside the `"Steam"` section (`Software -> Valve -> Steam`) so per-app
    /// `"apps" -> "<appid>" -> "language"` entries in `config.vdf` never shadow the client UI language.
    pub fn parse_language_from_vdf_text(content: &str) -> Option<String> {
        let mut section_stack: Vec<String> = Vec::new();
        let mut pending_section: Option<String> = None;
        let mut fallback_lang: Option<String> = None;

        for raw_line in content.lines() {
            let line = match raw_line.find("//") {
                Some(idx) => &raw_line[..idx],
                None => raw_line,
            }
            .trim();
            if line.is_empty() {
                continue;
            }

            let tokens = Self::extract_vdf_line_tokens(line);
            for tok in tokens {
                match tok {
                    VdfLineToken::OpenBrace => {
                        let sec = pending_section
                            .take()
                            .unwrap_or_default()
                            .to_ascii_lowercase();
                        section_stack.push(sec);
                    }
                    VdfLineToken::CloseBrace => {
                        pending_section = None;
                        let _ = section_stack.pop();
                    }
                    VdfLineToken::KeyOnly(k) => {
                        pending_section = Some(k);
                    }
                    VdfLineToken::KeyValue(k, v) => {
                        pending_section = None;
                        if k.eq_ignore_ascii_case("language") {
                            if let Some(normalized) = Self::normalize_steam_language_token(&v) {
                                let in_apps_or_depots = section_stack
                                    .iter()
                                    .any(|s| s == "apps" || s == "depots");
                                let in_steam_section = section_stack
                                    .last()
                                    .map(|s| s == "steam")
                                    .unwrap_or(false)
                                    && !in_apps_or_depots;
                                if in_steam_section {
                                    return Some(normalized);
                                }
                                if !in_apps_or_depots && fallback_lang.is_none() {
                                    fallback_lang = Some(normalized);
                                }
                            }
                        }
                    }
                }
            }
        }

        if fallback_lang.is_some() {
            return fallback_lang;
        }

        let re = regex::Regex::new(r#"(?i)"language"\s+"([^"]+)""#).ok()?;
        re.captures(content)
            .and_then(|c| c.get(1))
            .and_then(|m| Self::normalize_steam_language_token(m.as_str()))
    }

    fn normalize_steam_language_token(raw: &str) -> Option<String> {
        let trimmed = raw.trim().to_ascii_lowercase();
        if trimmed.is_empty() {
            return None;
        }
        if trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            Some(trimmed)
        } else {
            None
        }
    }

    fn extract_vdf_line_tokens(line: &str) -> Vec<VdfLineToken> {
        let mut out = Vec::new();
        let bytes = line.as_bytes();
        let mut i = 0;
        let mut strings_on_line: Vec<String> = Vec::new();

        let flush_strings = |strings: &mut Vec<String>, out: &mut Vec<VdfLineToken>| {
            let mut idx = 0;
            while idx < strings.len() {
                if idx + 1 < strings.len() {
                    out.push(VdfLineToken::KeyValue(
                        strings[idx].clone(),
                        strings[idx + 1].clone(),
                    ));
                    idx += 2;
                } else {
                    out.push(VdfLineToken::KeyOnly(strings[idx].clone()));
                    idx += 1;
                }
            }
            strings.clear();
        };

        while i < bytes.len() {
            let b = bytes[i];
            if b.is_ascii_whitespace() {
                i += 1;
                continue;
            }
            if b == b'{' {
                flush_strings(&mut strings_on_line, &mut out);
                out.push(VdfLineToken::OpenBrace);
                i += 1;
                continue;
            }
            if b == b'}' {
                flush_strings(&mut strings_on_line, &mut out);
                out.push(VdfLineToken::CloseBrace);
                i += 1;
                continue;
            }
            if b == b'"' {
                i += 1;
                let mut buf = String::new();
                while i < bytes.len() {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        if let Some(ch) = line[i + 1..].chars().next() {
                            buf.push(ch);
                            i += 1 + ch.len_utf8();
                            continue;
                        }
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    if let Some(ch) = line[i..].chars().next() {
                        buf.push(ch);
                        i += ch.len_utf8();
                    } else {
                        break;
                    }
                }
                strings_on_line.push(buf);
                continue;
            }
            i += 1;
        }
        flush_strings(&mut strings_on_line, &mut out);
        out
    }

    /// Reads Steam UI language from `config/config.vdf` (primary) or `registry.vdf` (fallback).
    pub fn detect_steam_language(override_root: Option<&Path>) -> Option<String> {
        let mut candidate_files = Vec::new();

        if let Some(root) = override_root.filter(|p| !p.as_os_str().is_empty()) {
            candidate_files.push(root.join("config").join("config.vdf"));
            candidate_files.push(root.join("registry.vdf"));
        } else {
            let cfg_override = crate::services::settings_service::SettingsService::default()
                .settings
                .steam_path_override;
            if let Some(ov_root) = Some(cfg_override.trim())
                .filter(|s| !s.is_empty())
                .and_then(expand_tilde)
            {
                candidate_files.push(ov_root.join("config").join("config.vdf"));
                candidate_files.push(ov_root.join("registry.vdf"));
            }

            if let Some(home) = dirs::home_dir() {
                candidate_files.push(home.join(".local/share/Steam/config/config.vdf"));
                candidate_files.push(home.join(".steam/steam/config/config.vdf"));
                candidate_files.push(home.join(".steam/root/config/config.vdf"));
                candidate_files.push(home.join(".steam/debian-installation/config/config.vdf"));
                candidate_files.push(
                    home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/config/config.vdf"),
                );
                candidate_files.push(home.join(".steam/registry.vdf"));
                candidate_files.push(home.join(".local/share/Steam/registry.vdf"));
                candidate_files.push(home.join(".steam/steam/registry.vdf"));
                candidate_files.push(home.join(".steam/debian-installation/registry.vdf"));
                candidate_files.push(
                    home.join(".var/app/com.valvesoftware.Steam/.steam/registry.vdf"),
                );
            }
        }

        for p in candidate_files {
            if p.is_file() {
                if let Ok(raw) = fs::read_to_string(&p) {
                    if let Some(lang) = Self::parse_language_from_vdf_text(&raw) {
                        if !lang.is_empty() {
                            return Some(lang);
                        }
                    }
                }
            }
        }
        None
    }

    /// Detects Steam Store API language token (`l=<lang>`) from `config.vdf` / `registry.vdf`,
    /// falling back to `"english"` when no Steam language is configured or readable.
    pub fn detect_steam_store_language(override_root: Option<&Path>) -> String {
        Self::detect_steam_language(override_root).unwrap_or_else(|| "english".to_string())
    }

    /// Instance method returning the Steam Store API language token (`"english"`, `"schinese"`, etc.).
    pub fn store_language(&self) -> String {
        let root = self.installation.as_ref().map(|i| i.root.as_path());
        Self::detect_steam_store_language(root)
    }

    /// Maps Steam's internal language string (`schinese`, `russian`, `english`, etc.)
    /// to 2-letter UI language code (`zh`, `ru`, `en`, etc.).
    pub fn steam_lang_to_ui_code(steam_lang: &str) -> &'static str {
        match steam_lang.trim().to_ascii_lowercase().as_str() {
            "schinese" | "tchinese" | "chinese" | "zh" => "zh",
            "russian" | "ru" => "ru",
            "spanish" | "latam" | "es" => "es",
            "german" | "de" => "de",
            "french" | "fr" => "fr",
            "brazilian" | "portuguese" | "pt" => "pt",
            "japanese" | "ja" => "ja",
            "italian" | "it" => "it",
            "koreana" | "korean" | "ko" => "ko",
            "polish" | "pl" => "pl",
            "dutch" | "nl" => "nl",
            "turkish" | "tr" => "tr",
            "arabic" | "ar" => "ar",
            "thai" | "th" => "th",
            "vietnamese" | "vi" => "vi",
            "ukrainian" | "uk" => "uk",
            "czech" | "cs" => "cs",
            "hungarian" | "hu" => "hu",
            "romanian" | "ro" => "ro",
            "swedish" | "sv" => "sv",
            "danish" | "da" => "da",
            "finnish" | "fi" => "fi",
            "norwegian" | "no" => "no",
            "bulgarian" | "bg" => "bg",
            "greek" | "el" => "el",
            "indonesian" | "id" => "id",
            _ => "en",
        }
    }

    /// Detects Steam UI language and returns the normalized 2-letter code (e.g. "zh", "ru", "en").
    pub fn detect_steam_ui_language_code(override_root: Option<&Path>) -> Option<String> {
        Self::detect_steam_language(override_root)
            .map(|l| Self::steam_lang_to_ui_code(&l).to_string())
    }

    /// Instance method returning detected Steam UI language code.
    pub fn ui_language_code(&self) -> Option<String> {
        let root = self.installation.as_ref().map(|i| i.root.as_path());
        Self::detect_steam_ui_language_code(root)
    }
}

#[derive(Debug, Clone)]
enum VdfLineToken {
    OpenBrace,
    CloseBrace,
    KeyOnly(String),
    KeyValue(String, String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_steam_service_detection_and_query() {
        let mut svc = SteamService::new();
        let _ = svc.detect(None);
        let _ = svc.is_available();
        let _ = svc.scan_all_games();
    }

    #[test]
    fn test_parse_language_from_vdf() {
        let registry_vdf = r#"
"Registry"
{
    "HKCU"
    {
        "Software"
        {
            "Valve"
            {
                "Steam"
                {
                    "Language"		"schinese"
                }
            }
        }
    }
}
"#;
        assert_eq!(
            SteamService::parse_language_from_vdf_text(registry_vdf),
            Some("schinese".to_string())
        );
        assert_eq!(SteamService::steam_lang_to_ui_code("schinese"), "zh");
        assert_eq!(SteamService::steam_lang_to_ui_code("russian"), "ru");
        assert_eq!(SteamService::steam_lang_to_ui_code("english"), "en");
    }

    #[test]
    fn test_config_vdf_language_detection_ignores_per_app_override_and_falls_back_to_english() {
        let dir = tempfile::tempdir().unwrap();
        let steam_root = dir.path().join("Steam");
        fs::create_dir_all(steam_root.join("config")).unwrap();

        // Empty root without config.vdf falls back to "english"
        assert_eq!(SteamService::detect_steam_language(Some(&steam_root)), None);
        assert_eq!(
            SteamService::detect_steam_store_language(Some(&steam_root)),
            "english"
        );

        // config.vdf where per-app "apps" entry appears BEFORE Steam client "Language"
        let config_vdf = r#"
"InstallConfigStore"
{
    "Software"
    {
        "Valve"
        {
            "Steam"
            {
                "apps"
                {
                    "480"
                    {
                        "language"		"english"
                    }
                }
                "Language"		"japanese"
            }
        }
    }
}
"#;
        fs::write(steam_root.join("config/config.vdf"), config_vdf).unwrap();

        assert_eq!(
            SteamService::detect_steam_language(Some(&steam_root)),
            Some("japanese".to_string())
        );
        assert_eq!(
            SteamService::detect_steam_store_language(Some(&steam_root)),
            "japanese"
        );
        assert_eq!(
            SteamService::detect_steam_ui_language_code(Some(&steam_root)),
            Some("ja".to_string())
        );
    }
}
