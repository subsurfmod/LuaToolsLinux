//! LuaEditor - Syntax-highlighted Lua editor with live error checking.
//!
//! Provides live editing & validation for `.lua` stplug-in directives:
//! `addappid`, `setManifestid`, `addtoken`, and pin/unpin toggling.

use std::collections::HashSet;

use eframe::egui::{self, RichText, ScrollArea, TextEdit};

use crate::parsers::lua_parser::{LuaContents, LuaFileParser};
use crate::ui::theme::{lua_layouter, Palette};

/// Live validation summary for stplug-in `.lua` text.
#[derive(Debug, Clone)]
pub struct LuaValidationReport {
    pub parsed: LuaContents,
    pub error_message: Option<String>,
    pub warnings: Vec<String>,
}

/// Parse and validate `.lua` stplug-in text for syntax issues, malformed depot keys,
/// duplicate `addappid` declarations, and manifest pin status.
pub fn validate_lua_text(text: &str, app_id: u32) -> LuaValidationReport {
    let parsed = LuaFileParser::parse_text(text, app_id);
    let mut error_message: Option<String> = None;
    let mut warnings = Vec::new();

    // Basic line-level syntax checks (unclosed quotes or unbalanced parentheses outside comments)
    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let mut in_quote: Option<char> = None;
        let mut escaped = false;
        let mut paren_depth: i32 = 0;
        let mut chars = raw_line.chars().peekable();

        while let Some(ch) = chars.next() {
            if let Some(q) = in_quote {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == q {
                    in_quote = None;
                }
                continue;
            }
            if ch == '-' && chars.peek() == Some(&'-') {
                break; // Remainder of line is a Lua comment
            }
            if ch == '"' || ch == '\'' {
                in_quote = Some(ch);
            } else if ch == '(' {
                paren_depth += 1;
            } else if ch == ')' {
                paren_depth -= 1;
                if paren_depth < 0 {
                    error_message = Some(format!(
                        "Line {}: unexpected closing parenthesis ')'",
                        line_no
                    ));
                    break;
                }
            }
        }
        if error_message.is_some() {
            break;
        }
        if let Some(q) = in_quote {
            error_message = Some(format!("Line {}: unclosed string literal ({})", line_no, q));
            break;
        }
        if paren_depth > 0 {
            error_message = Some(format!(
                "Line {}: missing {} closing parenthesis ')'",
                line_no, paren_depth
            ));
            break;
        }
    }

    if parsed.entries.is_empty() {
        warnings.push("No addappid directives found in .lua file".into());
    }

    // Detect duplicate addappid(<id>, ...) directives in raw source lines
    // (LuaFileParser::parse_text deduplicates parsed.entries by ID).
    let mut seen = HashSet::new();
    let mut warned_dups = HashSet::new();
    for raw_line in text.lines() {
        let trimmed = raw_line.trim_start();
        if trimmed.starts_with("--") {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if let Some(after_fn) = lower.strip_prefix("addappid") {
            let after_ws = after_fn.trim_start();
            if let Some(inside) = after_ws.strip_prefix('(') {
                let digits: String = inside
                    .trim_start()
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(id) = digits.parse::<u32>() {
                    if !seen.insert(id) && warned_dups.insert(id) {
                        warnings.push(format!("Duplicate addappid for AppID {}", id));
                    }
                }
            }
        }
    }

    for entry in &parsed.entries {
        if let Some(ref key) = entry.key {
            let trimmed = key.trim();
            if trimmed.len() != 64 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
                warnings.push(format!(
                    "Depot {} has an invalid decryption key ({} chars; expected 64 hex chars)",
                    entry.id,
                    trimmed.len()
                ));
            }
        }
        if let Some(ref gid) = entry.manifest_id {
            if gid.is_empty() || !gid.chars().all(|c| c.is_ascii_digit()) {
                warnings.push(format!(
                    "Depot {} has a non-numeric manifest GID '{}'",
                    entry.id, gid
                ));
            }
        }
    }

    if !parsed.commented_pins.is_empty() {
        warnings.push(format!(
            "{} pinned manifest(s) are commented out",
            parsed.commented_pins.len()
        ));
    }

    LuaValidationReport {
        parsed,
        error_message,
        warnings,
    }
}

/// Lua editor with real-time parse validation and syntax highlighting.
#[derive(Default)]
pub struct LuaEditor {
    pub text: String,
    pub app_id: u32,
    pub parsed: Option<LuaContents>,
    pub edit_live: bool,
    pub error_message: Option<String>,
    pub warnings: Vec<String>,
}

impl LuaEditor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_text(&mut self, text: String, app_id: u32) {
        self.app_id = app_id;
        self.text = text;
        self.reparse();
    }

    pub fn reparse(&mut self) {
        let report = validate_lua_text(&self.text, self.app_id);
        self.parsed = Some(report.parsed);
        self.error_message = report.error_message;
        self.warnings = report.warnings;
    }

    /// Render the Lua editor with syntax highlighting and live validation.
    pub fn render(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;

        if self.parsed.is_none() && !self.text.is_empty() {
            self.reparse();
        }

        ui.spacing_mut().item_spacing.y = 8.0;

        if let Some(parsed) = &self.parsed {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} entries, {} depots, {} DLCs",
                        parsed.entries.len(),
                        parsed.depot_count(),
                        parsed.dlc_count()
                    ))
                    .size(12.0)
                    .color(Palette::TEXT_MUTED),
                );

                if parsed.has_active_pins() {
                    ui.label(
                        RichText::new(format!("{} active pins", parsed.active_pins.len()))
                            .size(12.0)
                            .color(Palette::GO_TEXT),
                    );
                }

                if !parsed.commented_pins.is_empty() {
                    ui.label(
                        RichText::new(format!("{} commented pins", parsed.commented_pins.len()))
                            .size(12.0)
                            .color(Palette::WARN_TEXT),
                    );
                }
            });
        }

        if let Some(ref err) = self.error_message {
            ui.colored_label(Palette::DANGER_TEXT, format!("Error: {}", err));
        }

        for warning in &self.warnings {
            ui.colored_label(Palette::WARN_TEXT, format!("Warning: {}", warning));
        }

        ui.add_space(4.0);

        let mut layouter =
            |ui: &egui::Ui, string: &str, wrap_width: f32| lua_layouter(ui, string, wrap_width);

        ScrollArea::vertical()
            .id_salt(("lua_editor_scroll", self.app_id))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let response = ui.add(
                    TextEdit::multiline(&mut self.text)
                        .code_editor()
                        .interactive(self.edit_live)
                        .desired_rows(20)
                        .desired_width(f32::INFINITY)
                        .frame(false)
                        .layouter(&mut layouter),
                );

                if response.changed() {
                    self.reparse();
                    changed = true;
                }
            });

        changed
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn parsed(&self) -> Option<&LuaContents> {
        self.parsed.as_ref()
    }

    pub fn has_errors(&self) -> bool {
        self.error_message.is_some()
    }

    /// Saves the current editor text to the live `stplug-in/<appid>.lua` file, updates or creates
    /// a `LuaVault` variant in-place (preserving `edit_base_hash` lineage instead of duplicating
    /// variants), and synchronizes `SLSsteam` `config.yaml` (LT-37 & LNX-02).
    pub fn save_to_vault_and_sync(
        &mut self,
        vault: &crate::models::lua_vault::LuaVault,
        sls: &mut crate::services::slssteam_service::SlssteamService,
    ) -> Result<crate::models::lua_vault::LuaVariant, String> {
        self.reparse();
        if !vault.write_live(self.app_id, &self.text) {
            return Err(format!(
                "Failed to write live stplug-in/{}.lua on disk.",
                self.app_id
            ));
        }
        let variant = if let Some(base_hash) = vault.get_edit_base(self.app_id) {
            vault
                .update_variant(self.app_id, &base_hash, &self.text)
                .map(|mut v| {
                    if v.label.is_none() {
                        vault.rename(self.app_id, &v.hash, Some("User Edited"));
                        v.label = Some("User Edited".to_string());
                    }
                    v
                })
        } else {
            vault.save_text(self.app_id, &self.text, Some("User Edited"))
        }
        .ok_or_else(|| "Failed to update LuaVault variant.".to_string())?;

        vault.set_edit_base(self.app_id, None);

        if let Some(ref parsed) = self.parsed {
            crate::services::lua_installer::LuaInstaller::sync_with_slssteam(
                sls,
                self.app_id,
                parsed,
            );
        }
        Ok(variant)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_clean_lua_without_errors_or_warnings() {
        let key = "a8f4c2e1".repeat(8);
        let text = format!(
            "addappid(1245620) -- ELDEN RING\naddappid(1245621, 1, \"{}\")\nsetManifestid(1245621, \"8492018394810293847\", 1024)\n",
            key
        );
        let report = validate_lua_text(&text, 1245620);
        assert!(report.error_message.is_none());
        assert!(report.warnings.is_empty());
        assert_eq!(report.parsed.entries.len(), 2);
        assert!(report.parsed.has_active_pins());
    }

    #[test]
    fn detects_unclosed_quote_and_unbalanced_parens() {
        let bad_quote = "addappid(1245621, 1, \"abcdef)\n";
        let r1 = validate_lua_text(bad_quote, 1245620);
        assert!(r1
            .error_message
            .as_deref()
            .unwrap_or("")
            .contains("unclosed string"));

        let bad_paren = "addappid(1245620\n";
        let r2 = validate_lua_text(bad_paren, 1245620);
        assert!(r2
            .error_message
            .as_deref()
            .unwrap_or("")
            .contains("closing parenthesis"));

        // Quotes or parens inside comments must not trigger syntax errors
        let commented = "addappid(1245620) -- don't fail on unmatched (quote\n";
        let r3 = validate_lua_text(commented, 1245620);
        assert!(r3.error_message.is_none());
    }

    #[test]
    fn deduplicates_duplicate_warnings_and_flags_bad_depot_key() {
        let text = "addappid(1245620)\naddappid(1245620)\naddappid(1245620)\naddappid(1245621, 1, \"deadbeef\")\n";
        let report = validate_lua_text(text, 1245620);
        let dup_count = report
            .warnings
            .iter()
            .filter(|w| w.contains("Duplicate addappid for AppID 1245620"))
            .count();
        assert_eq!(dup_count, 1);
        assert!(report
            .warnings
            .iter()
            .any(|w| w.contains("invalid decryption key")));
    }

    #[test]
    fn editor_set_text_updates_parsed_and_error_state() {
        let mut ed = LuaEditor::new();
        ed.set_text("addappid(730\n".into(), 730);
        assert!(ed.has_errors());
        assert!(ed.parsed().is_some());

        ed.set_text("addappid(730)\n".into(), 730);
        assert!(!ed.has_errors());
    }

    #[test]
    fn editor_save_to_vault_and_sync_updates_variant_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let steam_root = tmp.path().join("Steam");
        let stplug = steam_root.join("config/stplug-in");
        std::fs::create_dir_all(&stplug).unwrap();
        let inst = crate::models::steam::SteamInstallation {
            root: steam_root.clone(),
            libraries: vec![crate::models::steam::SteamLibrary::new(steam_root.clone())],
            config_path: steam_root.join("config"),
            stplug_in_path: stplug,
        };
        let vault = crate::models::lua_vault::LuaVault::new(
            Some(inst),
            Some(tmp.path().join("vault")),
        );
        let mut sls = crate::services::slssteam_service::SlssteamService::new(Some(
            tmp.path().join("sls.yaml"),
        ));

        let mut ed = LuaEditor::new();
        ed.set_text("addappid(1245620)\n".into(), 1245620);
        let v1 = ed.save_to_vault_and_sync(&vault, &mut sls).unwrap();
        assert_eq!(vault.get_variants(1245620).len(), 1);
        assert_eq!(vault.get_edit_base(1245620), None);

        // Subsequent save without edit_base saves text cleanly and clears base
        ed.set_text(
            "addappid(1245620)\naddappid(2778580) -- Shadow of the Erdtree\n".into(),
            1245620,
        );
        let v2 = ed.save_to_vault_and_sync(&vault, &mut sls).unwrap();
        assert_ne!(v1.hash, v2.hash);
        assert_eq!(vault.get_variants(1245620).len(), 2);
        assert_eq!(vault.get_edit_base(1245620), None);
        assert!(sls.config.additional_apps.contains(&1245620));
        assert!(sls
            .config
            .dlc_data
            .get(&1245620)
            .map(|m| m.contains_key(&2778580))
            .unwrap_or(false));
    }
}
