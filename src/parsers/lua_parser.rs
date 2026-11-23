//! Lua file parser & manipulator (`addappid`, `setManifestid`, `addtoken`, pin toggling).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaEntry {
    pub id: u32,
    pub key: Option<String>,
    pub manifest_id: Option<String>,
    pub commented_manifest_id: Option<String>,
    pub comment: Option<String>,
    pub size_on_disk: Option<u64>,
}

impl LuaEntry {
    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    pub fn is_dlc(&self) -> bool {
        !self.has_key() && self.id != 0
    }
}

#[derive(Debug, Clone, Default)]
pub struct LuaContents {
    pub base_app_id: u32,
    pub entries: Vec<LuaEntry>,
    pub active_pins: BTreeMap<u32, String>,
    pub commented_pins: BTreeMap<u32, String>,
    pub disabled_entries: Vec<LuaEntry>,
    pub tokens: BTreeMap<u32, String>,
}

impl LuaContents {
    pub fn depot_count(&self) -> usize {
        self.entries.iter().filter(|e| e.has_key()).count()
    }

    pub fn dlc_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| !e.has_key() && e.id != self.base_app_id)
            .count()
    }

    pub fn has_active_pins(&self) -> bool {
        !self.active_pins.is_empty()
    }
}

fn add_appid_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)^addappid\s*\(\s*(\d+)\s*(?:,\s*(?:\d+\s*(?:,\s*["']([^"']*)["'])?|["']([^"']*)["']))?\s*\)\s*;?[ \t]*(?:--[ \t]*(.*?)[ \t]*)?$"#).unwrap()
    })
}

fn set_manifest_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)^setManifestid\s*\(\s*(\d+)\s*,\s*["']?(\d+)["']?\s*(?:,\s*(\d+))?\s*\)"#)
            .unwrap()
    })
}

fn comment_pin_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^(\s*)(setManifestid\s*\()").unwrap())
}

fn infer_appid_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)addappid\s*\(\s*(\d+)").unwrap())
}

fn add_token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)^addtoken\s*\(\s*(\d+)\s*,\s*["']([^"']+)["']\s*\)"#).unwrap()
    })
}

fn comment_tail_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s*\(\d+\)\s*\S*\s*$").unwrap())
}

pub struct LuaFileParser;

impl LuaFileParser {
    fn clean_comment(raw: Option<&str>) -> Option<String> {
        let s = raw?.trim();
        if s.is_empty() {
            return None;
        }
        let low = s.to_ascii_lowercase();
        if low.contains("setmanifestid") || low.contains("addappid") || low.contains("addtoken") {
            return None;
        }
        let cleaned = comment_tail_re().replace(s, "").trim().to_string();
        if cleaned.is_empty() {
            None
        } else {
            Some(cleaned)
        }
    }

    pub fn parse_file(path: &Path, app_id_from_name: u32) -> Option<LuaContents> {
        let text = fs::read_to_string(path).ok()?;
        Some(Self::parse_text(&text, app_id_from_name))
    }

    pub fn parse_text(text: &str, app_id_from_name: u32) -> LuaContents {
        let mut order: Vec<u32> = Vec::new();
        let mut key_by_id: BTreeMap<u32, Option<String>> = BTreeMap::new();
        let mut comment_by_id: BTreeMap<u32, String> = BTreeMap::new();
        let mut manifests: BTreeMap<u32, String> = BTreeMap::new();
        let mut commented_manifests: BTreeMap<u32, String> = BTreeMap::new();
        let mut sizes: BTreeMap<u32, u64> = BTreeMap::new();
        let mut disabled_order: Vec<u32> = Vec::new();
        let mut disabled_key_by_id: BTreeMap<u32, Option<String>> = BTreeMap::new();
        let mut tokens: BTreeMap<u32, String> = BTreeMap::new();

        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }

            let commented = line.starts_with("--");
            let mut line_to_match = if commented { line[2..].trim() } else { line };
            while line_to_match.starts_with('-') {
                line_to_match = line_to_match[1..].trim();
            }

            if !commented {
                if let Some(cap) = add_token_re().captures(line_to_match) {
                    if let (Some(id_m), Some(tok_m)) = (cap.get(1), cap.get(2)) {
                        if let Ok(aid) = id_m.as_str().parse::<u32>() {
                            tokens.insert(aid, tok_m.as_str().to_string());
                        }
                    }
                }
            }

            if let Some(pin) = set_manifest_re().captures(line_to_match) {
                if let (Some(d_m), Some(gid_m)) = (pin.get(1), pin.get(2)) {
                    if let Ok(depot) = d_m.as_str().parse::<u32>() {
                        let gid = gid_m.as_str().to_string();
                        if commented {
                            commented_manifests.insert(depot, gid);
                        } else {
                            manifests.insert(depot, gid);
                        }
                        if let Some(sz_m) = pin.get(3) {
                            if let Ok(sz) = sz_m.as_str().parse::<u64>() {
                                if sz > 0 {
                                    sizes.insert(depot, sz);
                                }
                            }
                        }
                    }
                }
            }

            let Some(m) = add_appid_re().captures(line_to_match) else {
                continue;
            };
            let Ok(entry_id) = m.get(1).unwrap().as_str().parse::<u32>() else {
                continue;
            };

            let key = m
                .get(2)
                .or_else(|| m.get(3))
                .map(|k| k.as_str().to_string())
                .filter(|k| !k.is_empty());
            let comment = Self::clean_comment(m.get(4).map(|c| c.as_str()));

            if let Some(c) = comment {
                let replace = comment_by_id
                    .get(&entry_id)
                    .map(|old| c.len() > old.len())
                    .unwrap_or(true);
                if replace {
                    comment_by_id.insert(entry_id, c);
                }
            }

            if commented {
                if let Some(existing) = disabled_key_by_id.get_mut(&entry_id) {
                    if existing.is_none() && key.is_some() {
                        *existing = key;
                    }
                } else {
                    disabled_key_by_id.insert(entry_id, key);
                    disabled_order.push(entry_id);
                }
                continue;
            }

            if let Some(existing) = key_by_id.get_mut(&entry_id) {
                if existing.is_none() && key.is_some() {
                    *existing = key;
                }
            } else {
                key_by_id.insert(entry_id, key);
                order.push(entry_id);
            }
        }

        let entries: Vec<LuaEntry> = order
            .iter()
            .map(|&id| LuaEntry {
                id,
                key: key_by_id.get(&id).cloned().flatten(),
                manifest_id: manifests.get(&id).cloned(),
                commented_manifest_id: commented_manifests.get(&id).cloned(),
                comment: comment_by_id.get(&id).cloned(),
                size_on_disk: sizes.get(&id).copied(),
            })
            .collect();

        let disabled_entries: Vec<LuaEntry> = disabled_order
            .into_iter()
            .filter(|id| !key_by_id.contains_key(id))
            .map(|id| LuaEntry {
                id,
                key: disabled_key_by_id.get(&id).cloned().flatten(),
                manifest_id: manifests.get(&id).cloned(),
                commented_manifest_id: commented_manifests.get(&id).cloned(),
                comment: comment_by_id.get(&id).cloned(),
                size_on_disk: sizes.get(&id).copied(),
            })
            .collect();

        let base_app_id = entries.first().map(|e| e.id).unwrap_or(app_id_from_name);

        LuaContents {
            base_app_id,
            entries,
            active_pins: manifests,
            commented_pins: commented_manifests,
            disabled_entries,
            tokens,
        }
    }

    pub fn comment_out_manifest_pins(lua_text: &str) -> String {
        let re = comment_pin_re();
        let mut out: Vec<String> = Vec::new();
        for line in lua_text.lines() {
            if line.trim_start().starts_with("--") {
                out.push(line.to_string());
            } else {
                out.push(re.replace(line, "${1}-- ${2}").to_string());
            }
        }
        let mut joined = out.join("\n");
        if lua_text.ends_with('\n') {
            joined.push('\n');
        }
        joined
    }

    pub fn toggle_manifest_pin_in_text(
        text: &str,
        depot_id: u32,
        fallback_manifest_id: Option<&str>,
    ) -> (String, bool) {
        let pattern = format!(
            r"(?i)^(\s*)(--\s*)?(setManifestid\s*\(\s*{}\s*,.*)$",
            depot_id
        );
        let Ok(re) = Regex::new(&pattern) else {
            return (text.to_string(), false);
        };

        let mut changed = false;
        let mut new_lines: Vec<String> = Vec::new();
        for line in text.lines() {
            if let Some(cap) = re.captures(line) {
                let indent = cap.get(1).map(|m| m.as_str()).unwrap_or("");
                let is_commented = cap.get(2).is_some();
                let body = cap.get(3).map(|m| m.as_str()).unwrap_or("");
                if is_commented {
                    new_lines.push(format!("{}{}", indent, body));
                } else {
                    new_lines.push(format!("{}-- {}", indent, body));
                }
                changed = true;
            } else {
                new_lines.push(line.to_string());
            }
        }

        if !changed {
            if let Some(gid) = fallback_manifest_id.filter(|s| !s.is_empty()) {
                new_lines.push(format!("setManifestid({}, \"{}\")", depot_id, gid));
                changed = true;
            }
        }

        let mut res = new_lines.join("\n");
        if text.ends_with('\n') || text.is_empty() {
            res.push('\n');
        }
        (res, changed)
    }

    pub fn infer_app_id_from_text(lua_text: &str) -> Option<u32> {
        let re = infer_appid_re();
        for line in lua_text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("--") {
                continue;
            }
            if let Some(cap) = re.captures(trimmed) {
                if let Ok(aid) = cap.get(1)?.as_str().parse::<u32>() {
                    if aid > 0 && aid < 2_000_000_000 {
                        return Some(aid);
                    }
                }
            }
        }
        None
    }

    pub fn default_lua_template(app_id: u32, game_name: &str) -> String {
        let header = if game_name.trim().is_empty() {
            String::new()
        } else {
            format!(" -- {}", game_name.trim())
        };
        format!(
            "-- LuaTools stplug-in configuration for AppID {app_id}{header}\n\
             addappid({app_id}){header}\n\n\
             -- Content depot with decryption key & optional pinned manifest:\n\
             -- addappid({next_id}, 1, \"64_CHAR_HEX_DECRYPTION_KEY\")\n\
             -- setManifestid({next_id}, \"MANIFEST_GID\", 0)\n",
            app_id = app_id,
            next_id = app_id.saturating_add(1),
            header = header
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_and_double_quotes_pins_and_tokens() {
        let sample = "\
            addappid(1245620) -- ELDEN RING\n\
            addappid(1245621, 1, '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'); -- Core\n\
            setManifestid(1245621, '8888777766665555444', 52428800000)\n\
            addappid(1245622, 1, \"fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210\")\n\
            addappid(1245623, \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\")\n\
            -- setManifestid(1245622, \"1111222233334444555\", 2048)\n\
            addappid(2778580) -- DLC\n\
            addtoken(1245620, \"secret_tok\")\n";

        let parsed = LuaFileParser::parse_text(sample, 1245620);
        assert_eq!(parsed.base_app_id, 1245620);
        assert_eq!(parsed.depot_count(), 3);
        assert_eq!(parsed.dlc_count(), 1);
        assert_eq!(
            parsed.active_pins.get(&1245621).map(String::as_str),
            Some("8888777766665555444")
        );
        assert_eq!(
            parsed.commented_pins.get(&1245622).map(String::as_str),
            Some("1111222233334444555")
        );
        assert_eq!(
            parsed.tokens.get(&1245620).map(String::as_str),
            Some("secret_tok")
        );

        let (t1, ch1) = LuaFileParser::toggle_manifest_pin_in_text(sample, 1245621, None);
        assert!(ch1);
        let p1 = LuaFileParser::parse_text(&t1, 1245620);
        assert!(!p1.active_pins.contains_key(&1245621));
        assert!(p1.commented_pins.contains_key(&1245621));

        let (t2, ch2) = LuaFileParser::toggle_manifest_pin_in_text(&t1, 1245621, None);
        assert!(ch2);
        let p2 = LuaFileParser::parse_text(&t2, 1245620);
        assert!(p2.active_pins.contains_key(&1245621));
    }
}
