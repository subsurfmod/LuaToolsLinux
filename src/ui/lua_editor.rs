//! LuaEditor - Syntax-highlighted Lua editor with error checking
//!
//! Provides syntax highlighting for .lua stplug-in directives:
//! addappid, setManifestid, addtoken, pin/unpin toggling

use eframe::egui::{
    self, Align, Color32, FontId, Margin, RichText, ScrollArea, Sense, TextEdit, Vec2,
};

use crate::parsers::lua_parser::{LuaContents, LuaFileParser};

/// Syntax-highlighted Lua editor with real-time error checking
pub struct LuaEditor {
    pub text: String,
    pub parsed: Option<LuaContents>,
    pub edit_live: bool,
    pub error_message: Option<String>,
    pub warnings: Vec<String>,
}

impl Default for LuaEditor {
    fn default() -> Self {
        Self {
            text: String::new(),
            parsed: None,
            edit_live: false,
            error_message: None,
            warnings: Vec::new(),
        }
    }
}

impl LuaEditor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_text(&mut self, text: String, app_id: u32) {
        self.text = text.clone();
        self.parsed = LuaFileParser::parse_text(&text, app_id).into();
        self.validate();
    }

    fn validate(&mut self) {
        self.error_message = None;
        self.warnings.clear();

        if let Some(parsed) = &self.parsed {
            // Check for common issues
            if parsed.entries.is_empty() {
                self.warnings.push("No addappid directives found in .lua file".into());
            }

            // Check for duplicate entries
            let mut seen = std::collections::HashSet::new();
            for entry in &parsed.entries {
                if !seen.insert(entry.id) {
                    self.warnings.push(format!("Duplicate addappid for AppID {}", entry.id));
                }
            }

            // Check for commented-out pins that might be intentional
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

    /// Render the Lua editor with syntax highlighting
    pub fn render(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;

        ui.spacing_mut().item_spacing.y = 8.0;

        // Status bar
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
                        RichText::new(format!(
                            "{} commented pins",
                            parsed.commented_pins.len()
                        ))
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
            ui.colored_label(Color32::from_rgb(255, 200, 100), format!("Warning: {}", warning));
        }

        ui.add_space(4.0);

        // Editor area - simple text editor without custom layout
        let mut changed = false;
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
                    self.validate();
                    changed = true;
                }
            });

        changed
    }

    /// Get the current edited text
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Get parsed contents
    pub fn parsed(&self) -> Option<&LuaContents> {
        self.parsed.as_ref()
    }

    /// Check if there are any errors
    pub fn has_errors(&self) -> bool {
        self.error_message.is_some()
    }
}