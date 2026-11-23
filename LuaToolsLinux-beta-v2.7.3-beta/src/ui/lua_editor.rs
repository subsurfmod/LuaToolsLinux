//! LuaEditor - Syntax-highlighted Lua editor with live error checking.
//!
//! Provides live editing & validation for `.lua` stplug-in directives:
//! `addappid`, `setManifestid`, `addtoken`, and pin/unpin toggling.

use eframe::egui::{self, Color32, RichText, ScrollArea, TextEdit};

use crate::parsers::lua_parser::{LuaContents, LuaFileParser};

/// Lua editor with real-time parse validation.
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

    fn reparse(&mut self) {
        self.parsed = Some(LuaFileParser::parse_text(&self.text, self.app_id));
        self.validate();
    }

    fn validate(&mut self) {
        self.error_message = None;
        self.warnings.clear();

        if let Some(parsed) = &self.parsed {
            if parsed.entries.is_empty() {
                self.warnings
                    .push("No addappid directives found in .lua file".into());
            }

            let mut seen = std::collections::HashSet::new();
            for entry in &parsed.entries {
                if !seen.insert(entry.id) {
                    self.warnings
                        .push(format!("Duplicate addappid for AppID {}", entry.id));
                }
            }

            if !parsed.commented_pins.is_empty() {
                self.warnings.push(format!(
                    "{} pinned manifest(s) are commented out",
                    parsed.commented_pins.len()
                ));
            }
        } else {
            self.error_message = Some("Failed to parse .lua file".into());
        }
    }

    /// Render the Lua editor with live validation.
    pub fn render(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;

        ui.spacing_mut().item_spacing.y = 8.0;

        if let Some(parsed) = &self.parsed {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} entries, {} depots, {} DLCs",
                        parsed.entries.len(),
                        parsed.depot_count(),
                        parsed.dlc_count()
                    ))
                    .size(12.0)
                    .color(Color32::from_gray(150)),
                );

                if parsed.has_active_pins() {
                    ui.label(
                        RichText::new(format!("{} active pins", parsed.active_pins.len()))
                            .size(12.0)
                            .color(Color32::from_rgb(100, 200, 100)),
                    );
                }

                if !parsed.commented_pins.is_empty() {
                    ui.label(
                        RichText::new(format!("{} commented pins", parsed.commented_pins.len()))
                            .size(12.0)
                            .color(Color32::from_rgb(200, 150, 100)),
                    );
                }
            });
        }

        if let Some(ref err) = self.error_message {
            ui.colored_label(Color32::from_rgb(255, 100, 100), format!("Error: {}", err));
        }

        for warning in &self.warnings {
            ui.colored_label(
                Color32::from_rgb(255, 200, 100),
                format!("Warning: {}", warning),
            );
        }

        ui.add_space(4.0);

        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let response = ui.add(
                    TextEdit::multiline(&mut self.text)
                        .code_editor()
                        .interactive(self.edit_live)
                        .desired_rows(20)
                        .desired_width(f32::INFINITY)
                        .frame(false),
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
}
