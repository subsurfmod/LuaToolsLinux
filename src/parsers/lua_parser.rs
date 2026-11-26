//! Lua file parser & manipulator (`addappid`, `setManifestid`, `addtoken`, pin toggling).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::steam_bridge::ost_platform::numbers::{parse_uint32, parse_uint64};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LuaEntry {
    pub id: u32,
    pub key: Option<String>,
    pub manifest_id: Option<String>,
    pub commented_manifest_id: Option<String>,
    pub comment: Option<String>,
    pub size_on_disk: Option<u64>,
    // Platform compatibility fields
    pub os: Option<String>,           // "linux", "windows", "macos"
    pub arch: Option<String>,         // "x86_64", "arm64"
    pub proton: Option<String>,       // Proton version hint: "GE-Proton", "Proton-Experimental", "Proton-Stable", "Proton-GE-9-25", etc.
    pub native: bool,                 // true if native Linux build exists
}

impl LuaEntry {
    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    pub fn is_dlc(&self) -> bool {
        !self.has_key() && self.id != 0
    }

    /// Check if this entry matches the target platform
    pub fn matches_platform(&self, target_os: &str, target_arch: &str) -> bool {
        if let Some(ref os) = self.os {
            if os != target_os {
                return false;
            }
        }
        if let Some(ref arch) = self.arch {
            if arch != target_arch {
                return false;
            }
        }
        true
    }

    /// Get the recommended Proton version for this entry
    pub fn get_proton_version(&self) -> Option<String> {
        self.proton.clone()
    }

    /// Check if this is a native Linux depot
    pub fn is_native_linux(&self) -> bool {
        self.native || self.os.as_deref() == Some("linux")
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
    pub process_map: BTreeMap<u32, String>,
    pub force_denuvo: BTreeSet<u32>,
    pub pinned_apps: BTreeSet<u32>,
    pub legacy_cd_keys: BTreeMap<u32, String>,
    pub app_tickets: BTreeMap<u32, String>,
    pub e_tickets: BTreeMap<u32, String>,
    pub eticket_urls: Vec<String>,
    pub stats: BTreeMap<u32, String>,
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

fn add_process_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)^addprocess\s*\(\s*(\d+)\s*,\s*["']([^"']+)["']\s*\)"#).unwrap()
    })
}

fn force_denuvo_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)^forcedenuvo\s*\(\s*(\d+)\s*\)"#).unwrap()
    })
}

fn pin_app_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)^pinApp\s*\(\s*(\d+)\s*\)"#).unwrap()
    })
}

fn set_legacy_cdkey_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)^setlegacycdkey\s*\(\s*(\d+)\s*,\s*["']([^"']+)["']\s*\)"#).unwrap()
    })
}

fn set_app_ticket_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)^setAppTicket\s*\(\s*(\d+)\s*,\s*["']([^"']+)["']\s*\)"#).unwrap()
    })
}

fn set_e_ticket_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)^setETicket\s*\(\s*(\d+)\s*,\s*["']([^"']+)["']\s*\)"#).unwrap()
    })
}

fn set_eticket_url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)^seteticketurl\s*\(\s*["']([^"']+)["']\s*\)"#).unwrap()
    })
}

fn set_stat_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)^setStat\s*\(\s*(\d+)\s*,\s*["']?([^"')\s]+)["']?\s*\)"#).unwrap()
    })
}

fn set_os_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)^setOS\s*\(\s*(\d+)\s*,\s*["']([^"']+)["']\s*\)"#).unwrap()
    })
}

fn set_arch_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)^setArch\s*\(\s*(\d+)\s*,\s*["']([^"']+)["']\s*\)"#).unwrap()
    })
}

fn set_proton_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)^setProton\s*\(\s*(\d+)\s*,\s*["']([^"']+)["']\s*\)"#).unwrap()
    })
}

fn set_native_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)^setNative\s*\(\s*(\d+)\s*\)"#).unwrap()
    })
}

fn comment_tail_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s*\(\d+\)\s*\S*\s*$").unwrap())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LuaVal {
    Num(i128),
    Str(String),
    Table(Vec<LuaVal>),
}

impl LuaVal {
    fn as_concat_str(&self) -> Option<String> {
        match self {
            LuaVal::Num(n) => Some(n.to_string()),
            LuaVal::Str(s) => Some(s.clone()),
            LuaVal::Table(_) => None,
        }
    }

    fn format_arg(&self) -> Option<String> {
        match self {
            LuaVal::Num(n) => Some(n.to_string()),
            LuaVal::Str(s) => Some(format!("\"{}\"", s)),
            LuaVal::Table(_) => None,
        }
    }
}

fn strip_inline_lua_comment(line: &str) -> (&str, Option<&str>) {
    let bytes = line.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && (in_single || in_double) {
            i += 2;
            continue;
        }
        if b == b'\'' && !in_double {
            in_single = !in_single;
        } else if b == b'"' && !in_single {
            in_double = !in_double;
        } else if !in_single && !in_double && b == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            return (line[..i].trim_end(), Some(&line[i..]));
        }
        i += 1;
    }
    (line, None)
}

fn split_top_level<'a>(s: &'a str, sep: &str) -> Vec<&'a str> {
    let mut parts = Vec::new();
    let bytes = s.as_bytes();
    let sep_bytes = sep.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut depth_paren = 0i32;
    let mut depth_brace = 0i32;
    let mut start = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && (in_single || in_double) {
            i += 2;
            continue;
        }
        if b == b'\'' && !in_double {
            in_single = !in_single;
        } else if b == b'"' && !in_single {
            in_double = !in_double;
        } else if !in_single && !in_double {
            match b {
                b'(' => depth_paren += 1,
                b')' => depth_paren -= 1,
                b'{' => depth_brace += 1,
                b'}' => depth_brace -= 1,
                _ => {}
            }
            if depth_paren == 0
                && depth_brace == 0
                && i + sep_bytes.len() <= bytes.len()
                && &bytes[i..i + sep_bytes.len()] == sep_bytes
            {
                parts.push(s[start..i].trim());
                i += sep_bytes.len();
                start = i;
                continue;
            }
        }
        i += 1;
    }
    parts.push(s[start..].trim());
    parts
}

fn is_valid_lua_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn eval_lua_expr(raw: &str, vars: &BTreeMap<String, LuaVal>) -> Option<LuaVal> {
    let expr = raw.trim();
    if expr.is_empty() {
        return None;
    }

    // 1. Table literal `{ ... }`
    if expr.starts_with('{') && expr.ends_with('}') {
        let inner = expr[1..expr.len() - 1].trim();
        if inner.is_empty() {
            return Some(LuaVal::Table(Vec::new()));
        }
        let mut items = Vec::new();
        for elem in split_top_level(inner, ",") {
            if elem.is_empty() {
                continue;
            }
            items.push(eval_lua_expr(elem, vars)?);
        }
        return Some(LuaVal::Table(items));
    }

    // 2. String concatenation `a .. b`
    let concat_parts = split_top_level(expr, "..");
    if concat_parts.len() > 1 {
        let mut out = String::new();
        for p in concat_parts {
            let v = eval_lua_expr(p, vars)?;
            out.push_str(&v.as_concat_str()?);
        }
        return Some(LuaVal::Str(out));
    }

    // 3. String literal `"..."` or `'...'`
    if (expr.starts_with('"') && expr.ends_with('"') && expr.len() >= 2)
        || (expr.starts_with('\'') && expr.ends_with('\'') && expr.len() >= 2)
    {
        return Some(LuaVal::Str(expr[1..expr.len() - 1].to_string()));
    }

    // 4. Direct integer literal
    if let Ok(n) = expr.parse::<i128>() {
        return Some(LuaVal::Num(n));
    }

    // 5. Simple integer addition / subtraction (`app + 1`, `base - 2`)
    let plus_parts = split_top_level(expr, "+");
    if plus_parts.len() > 1 && plus_parts.iter().all(|p| !p.is_empty()) {
        let mut sum = 0i128;
        for p in plus_parts {
            match eval_lua_expr(p, vars)? {
                LuaVal::Num(n) => sum = sum.saturating_add(n),
                _ => return None,
            }
        }
        return Some(LuaVal::Num(sum));
    }

    // 6. Variable lookup
    if is_valid_lua_ident(expr) {
        return vars.get(expr).cloned();
    }

    None
}

fn is_supported_directive_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "addappid"
            | "setmanifestid"
            | "addtoken"
            | "setlegacycdkey"
            | "setappticket"
            | "seteticket"
            | "seteticketurl"
            | "setstat"
            | "addprocess"
            | "forcedenuvo"
            | "pinapp"
            | "setos"
            | "setarch"
            | "setproton"
            | "setnative"
    )
}

fn try_substitute_directive_call(stmt: &str, vars: &BTreeMap<String, LuaVal>) -> Option<String> {
    let trimmed = stmt.trim().trim_end_matches(';').trim();
    let open_idx = trimmed.find('(')?;
    let close_idx = trimmed.rfind(')')?;
    if close_idx <= open_idx || close_idx != trimmed.len() - 1 {
        return None;
    }
    let func_name = trimmed[..open_idx].trim();
    if !is_supported_directive_name(func_name) {
        return None;
    }
    let args_str = &trimmed[open_idx + 1..close_idx];
    let raw_args = split_top_level(args_str, ",");
    let mut evaluated_args = Vec::with_capacity(raw_args.len());
    for arg in raw_args {
        if arg.is_empty() {
            continue;
        }
        if let Some(val) = eval_lua_expr(arg, vars) {
            if let Some(formatted) = val.format_arg() {
                evaluated_args.push(formatted);
                continue;
            }
        }
        evaluated_args.push(arg.to_string());
    }
    Some(format!("{}({})", func_name, evaluated_args.join(", ")))
}

fn process_procedural_stmt(
    raw_line: &str,
    vars: &mut BTreeMap<String, LuaVal>,
    out_lines: &mut Vec<String>,
) {
    let trimmed = raw_line.trim();
    if trimmed.is_empty() || trimmed.starts_with("--") {
        out_lines.push(raw_line.to_string());
        return;
    }

    let (code_part, comment_opt) = strip_inline_lua_comment(trimmed);
    let code = code_part.trim();
    if code.is_empty() {
        out_lines.push(raw_line.to_string());
        return;
    }

    // Check single-line `for ... do ... end`
    if let Some(expanded) = try_expand_for_block(code, vars) {
        out_lines.extend(expanded);
        return;
    }

    // Handle multiple semicolon-separated statements on one line
    let sub_stmts = split_top_level(code, ";");
    if sub_stmts.len() > 1 && sub_stmts.iter().filter(|s| !s.is_empty()).count() > 1 {
        for sub in sub_stmts {
            if !sub.is_empty() {
                process_procedural_stmt(sub, vars, out_lines);
            }
        }
        return;
    }

    let single = sub_stmts.first().copied().unwrap_or(code).trim();

    // Check `local var = expr` or `var = expr`
    let assign_target = single
        .strip_prefix("local ")
        .map(str::trim)
        .unwrap_or(single);
    if let Some(eq_idx) = assign_target.find('=') {
        let lhs = assign_target[..eq_idx].trim();
        let rhs = assign_target[eq_idx + 1..].trim();
        if is_valid_lua_ident(lhs) && !rhs.starts_with('=') {
            if let Some(val) = eval_lua_expr(rhs, vars) {
                vars.insert(lhs.to_string(), val);
                return;
            }
        }
    }

    // Substitute variables/concatenations in directive calls
    if let Some(mut substituted) = try_substitute_directive_call(single, vars) {
        if let Some(c) = comment_opt {
            substituted.push(' ');
            substituted.push_str(c);
        }
        out_lines.push(substituted);
        return;
    }

    out_lines.push(raw_line.to_string());
}

fn try_expand_for_block(
    block_code: &str,
    vars: &mut BTreeMap<String, LuaVal>,
) -> Option<Vec<String>> {
    let trimmed = block_code.trim();
    if !trimmed.starts_with("for ") && !trimmed.starts_with("for\t") {
        return None;
    }
    let without_for = trimmed[3..].trim();
    let do_pos = without_for
        .find(" do ")
        .or_else(|| without_for.find(" do\n"))
        .or_else(|| without_for.find("\ndo "))
        .or_else(|| {
            if without_for.ends_with(" do") {
                Some(without_for.len() - 3)
            } else {
                None
            }
        })?;
    let header = without_for[..do_pos].trim();
    let after_do = without_for[do_pos + 3..].trim();
    let body = after_do
        .strip_suffix("end")
        .map(str::trim)
        .filter(|_| {
            after_do == "end"
                || after_do.ends_with(" end")
                || after_do.ends_with("\nend")
                || after_do.ends_with("\tend")
                || after_do.ends_with(";end")
        })?;

    let mut out = Vec::new();

    // Case A: `for k, v in ipairs(...)` / `for _, id in pairs(...)`
    if let Some(in_idx) = header.find(" in ") {
        let vars_part = header[..in_idx].trim();
        let iter_part = header[in_idx + 4..].trim();
        let var_names: Vec<&str> = vars_part.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
        if var_names.is_empty() {
            return None;
        }

        let table_expr = if let Some(inner) = iter_part
            .strip_prefix("ipairs")
            .or_else(|| iter_part.strip_prefix("pairs"))
            .map(str::trim)
        {
            if inner.starts_with('(') && inner.ends_with(')') {
                inner[1..inner.len() - 1].trim()
            } else {
                inner
            }
        } else {
            iter_part
        };

        let LuaVal::Table(items) = eval_lua_expr(table_expr, vars)? else {
            return None;
        };

        for (idx, item) in items.into_iter().enumerate() {
            let mut loop_vars = vars.clone();
            if var_names.len() == 1 {
                loop_vars.insert(var_names[0].to_string(), item);
            } else {
                loop_vars.insert(var_names[0].to_string(), LuaVal::Num((idx + 1) as i128));
                loop_vars.insert(var_names[1].to_string(), item);
            }
            for line in body.lines() {
                process_procedural_stmt(line, &mut loop_vars, &mut out);
            }
        }
        return Some(out);
    }

    // Case B: Numeric `for i = start, stop[, step] do ... end`
    if let Some(eq_idx) = header.find('=') {
        let loop_var = header[..eq_idx].trim();
        if !is_valid_lua_ident(loop_var) {
            return None;
        }
        let bounds = split_top_level(&header[eq_idx + 1..], ",");
        if bounds.len() < 2 || bounds.len() > 3 {
            return None;
        }
        let LuaVal::Num(start_n) = eval_lua_expr(bounds[0], vars)? else {
            return None;
        };
        let LuaVal::Num(stop_n) = eval_lua_expr(bounds[1], vars)? else {
            return None;
        };
        let step_n = if bounds.len() == 3 {
            match eval_lua_expr(bounds[2], vars)? {
                LuaVal::Num(s) if s != 0 => s,
                _ => return None,
            }
        } else {
            1
        };

        let mut cur = start_n;
        let mut count = 0usize;
        while (step_n > 0 && cur <= stop_n) || (step_n < 0 && cur >= stop_n) {
            if count >= 4096 {
                break;
            }
            count += 1;
            let mut loop_vars = vars.clone();
            loop_vars.insert(loop_var.to_string(), LuaVal::Num(cur));
            for line in body.lines() {
                process_procedural_stmt(line, &mut loop_vars, &mut out);
            }
            cur = cur.saturating_add(step_n);
        }
        return Some(out);
    }

    None
}

/// Lightweight procedural Lua pre-evaluator (`BST-01`):
/// Evaluates `local var = ...` assignments, `"a" .. "b"` string concatenations,
/// and `for ... in ipairs({...}) do ... end` / numeric `for` loops into concrete directive lines.
pub fn expand_procedural_lua(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut vars: BTreeMap<String, LuaVal> = BTreeMap::new();
    let mut out_lines: Vec<String> = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0usize;

    while i < lines.len() {
        let raw_line = lines[i];
        let trimmed = raw_line.trim();
        if !trimmed.starts_with("--") && (trimmed.starts_with("for ") || trimmed.starts_with("for\t")) {
            let (code_first, _) = strip_inline_lua_comment(trimmed);
            // If not already closed with `end` on the same line, collect multi-line block
            let is_single_line_end = code_first.ends_with(" end")
                || code_first.ends_with(";end")
                || code_first.ends_with("\tend");
            if !is_single_line_end {
                let mut combined = String::from(code_first);
                let mut j = i + 1;
                let mut found_end = false;
                while j < lines.len() {
                    let (sub_code, _) = strip_inline_lua_comment(lines[j].trim());
                    combined.push('\n');
                    combined.push_str(sub_code.trim());
                    if sub_code.trim() == "end"
                        || sub_code.trim().ends_with(" end")
                        || sub_code.trim().ends_with(";end")
                    {
                        found_end = true;
                        break;
                    }
                    j += 1;
                }
                if found_end {
                    if let Some(expanded) = try_expand_for_block(&combined, &mut vars) {
                        out_lines.extend(expanded);
                        i = j + 1;
                        continue;
                    }
                }
            }
        }
        process_procedural_stmt(raw_line, &mut vars, &mut out_lines);
        i += 1;
    }

    out_lines.join("\n")
}

pub struct LuaFileParser;

impl LuaFileParser {
    pub fn parse_app_or_depot_id(raw: &str) -> Option<u32> {
        parse_uint32(raw.trim()).ok()
    }

    pub fn parse_manifest_id(raw: &str) -> Option<u64> {
        parse_uint64(raw.trim()).ok()
    }

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

    pub fn expand_procedural_lua(text: &str) -> String {
        expand_procedural_lua(text)
    }

    pub fn parse_file(path: &Path, app_id_from_name: u32) -> Option<LuaContents> {
        let text = fs::read_to_string(path).ok()?;
        Some(Self::parse_text(&text, app_id_from_name))
    }

    pub fn parse_text(text: &str, app_id_from_name: u32) -> LuaContents {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let expanded = expand_procedural_lua(text);
        let text = expanded.as_str();
        let mut order: Vec<u32> = Vec::new();
        let mut key_by_id: BTreeMap<u32, Option<String>> = BTreeMap::new();
        let mut comment_by_id: BTreeMap<u32, String> = BTreeMap::new();
        let mut manifests: BTreeMap<u32, String> = BTreeMap::new();
        let mut commented_manifests: BTreeMap<u32, String> = BTreeMap::new();
        let mut sizes: BTreeMap<u32, u64> = BTreeMap::new();
        let mut disabled_order: Vec<u32> = Vec::new();
        let mut disabled_key_by_id: BTreeMap<u32, Option<String>> = BTreeMap::new();
        let mut tokens: BTreeMap<u32, String> = BTreeMap::new();
        let mut process_map: BTreeMap<u32, String> = BTreeMap::new();
        let mut force_denuvo: BTreeSet<u32> = BTreeSet::new();
        let mut pinned_apps: BTreeSet<u32> = BTreeSet::new();
        let mut legacy_cd_keys: BTreeMap<u32, String> = BTreeMap::new();
        let mut app_tickets: BTreeMap<u32, String> = BTreeMap::new();
        let mut e_tickets: BTreeMap<u32, String> = BTreeMap::new();
        let mut eticket_urls: Vec<String> = Vec::new();
        let mut stats: BTreeMap<u32, String> = BTreeMap::new();

        // Platform compatibility tracking (collected during parse, applied when building entries)
        let mut entry_os: BTreeMap<u32, String> = BTreeMap::new();
        let mut entry_arch: BTreeMap<u32, String> = BTreeMap::new();
        let mut entry_proton: BTreeMap<u32, String> = BTreeMap::new();
        let mut entry_native: BTreeSet<u32> = BTreeSet::new();

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
                        if let Some(aid) = Self::parse_app_or_depot_id(id_m.as_str()) {
                            tokens.insert(aid, tok_m.as_str().to_string());
                        }
                    }
                }

                if let Some(cap) = add_process_re().captures(line_to_match) {
                    if let (Some(id_m), Some(exe_m)) = (cap.get(1), cap.get(2)) {
                        if let Some(aid) = Self::parse_app_or_depot_id(id_m.as_str()) {
                            process_map.insert(aid, exe_m.as_str().to_string());
                        }
                    }
                }

                if let Some(cap) = force_denuvo_re().captures(line_to_match) {
                    if let Some(id_m) = cap.get(1) {
                        if let Some(aid) = Self::parse_app_or_depot_id(id_m.as_str()) {
                            force_denuvo.insert(aid);
                        }
                    }
                }

                if let Some(cap) = pin_app_re().captures(line_to_match) {
                    if let Some(id_m) = cap.get(1) {
                        if let Some(aid) = Self::parse_app_or_depot_id(id_m.as_str()) {
                            pinned_apps.insert(aid);
                        }
                    }
                }

                if let Some(cap) = set_legacy_cdkey_re().captures(line_to_match) {
                    if let (Some(id_m), Some(k_m)) = (cap.get(1), cap.get(2)) {
                        if let Some(aid) = Self::parse_app_or_depot_id(id_m.as_str()) {
                            legacy_cd_keys.insert(aid, k_m.as_str().to_string());
                        }
                    }
                }

                if let Some(cap) = set_app_ticket_re().captures(line_to_match) {
                    if let (Some(id_m), Some(t_m)) = (cap.get(1), cap.get(2)) {
                        if let Some(aid) = Self::parse_app_or_depot_id(id_m.as_str()) {
                            app_tickets.insert(aid, t_m.as_str().to_string());
                        }
                    }
                }

                if let Some(cap) = set_e_ticket_re().captures(line_to_match) {
                    if let (Some(id_m), Some(t_m)) = (cap.get(1), cap.get(2)) {
                        if let Some(aid) = Self::parse_app_or_depot_id(id_m.as_str()) {
                            e_tickets.insert(aid, t_m.as_str().to_string());
                        }
                    }
                }

                if let Some(cap) = set_eticket_url_re().captures(line_to_match) {
                    if let Some(url_m) = cap.get(1) {
                        eticket_urls.push(url_m.as_str().to_string());
                    }
                }

                if let Some(cap) = set_stat_re().captures(line_to_match) {
                    if let (Some(id_m), Some(s_m)) = (cap.get(1), cap.get(2)) {
                        if let Some(aid) = Self::parse_app_or_depot_id(id_m.as_str()) {
                            stats.insert(aid, s_m.as_str().to_string());
                        }
                    }
                }

                if let Some(cap) = set_os_re().captures(line_to_match) {
                    if let (Some(id_m), Some(os_m)) = (cap.get(1), cap.get(2)) {
                        if let Some(aid) = Self::parse_app_or_depot_id(id_m.as_str()) {
                            entry_os.insert(aid, os_m.as_str().to_string());
                        }
                    }
                }

                if let Some(cap) = set_arch_re().captures(line_to_match) {
                    if let (Some(id_m), Some(arch_m)) = (cap.get(1), cap.get(2)) {
                        if let Some(aid) = Self::parse_app_or_depot_id(id_m.as_str()) {
                            entry_arch.insert(aid, arch_m.as_str().to_string());
                        }
                    }
                }

                if let Some(cap) = set_proton_re().captures(line_to_match) {
                    if let (Some(id_m), Some(proton_m)) = (cap.get(1), cap.get(2)) {
                        if let Some(aid) = Self::parse_app_or_depot_id(id_m.as_str()) {
                            entry_proton.insert(aid, proton_m.as_str().to_string());
                        }
                    }
                }

                if let Some(cap) = set_native_re().captures(line_to_match) {
                    if let Some(id_m) = cap.get(1) {
                        if let Some(aid) = Self::parse_app_or_depot_id(id_m.as_str()) {
                            entry_native.insert(aid);
                        }
                    }
                }
            }

            if let Some(pin) = set_manifest_re().captures(line_to_match) {
                if let (Some(d_m), Some(gid_m)) = (pin.get(1), pin.get(2)) {
                    if let (Some(depot), Some(_gid)) = (
                        Self::parse_app_or_depot_id(d_m.as_str()),
                        Self::parse_manifest_id(gid_m.as_str()),
                    ) {
                        let gid = gid_m.as_str().to_string();
                        if commented {
                            commented_manifests.insert(depot, gid);
                        } else {
                            manifests.insert(depot, gid);
                        }
                        if let Some(sz_m) = pin.get(3) {
                            if let Ok(sz) = parse_uint64(sz_m.as_str()) {
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
            let Some(id_m) = m.get(1) else {
                continue;
            };
            let Some(entry_id) = Self::parse_app_or_depot_id(id_m.as_str()) else {
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
                os: entry_os.get(&id).cloned(),
                arch: entry_arch.get(&id).cloned(),
                proton: entry_proton.get(&id).cloned(),
                native: entry_native.contains(&id),
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
                os: entry_os.get(&id).cloned(),
                arch: entry_arch.get(&id).cloned(),
                proton: entry_proton.get(&id).cloned(),
                native: entry_native.contains(&id),
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
            process_map,
            force_denuvo,
            pinned_apps,
            legacy_cd_keys,
            app_tickets,
            e_tickets,
            eticket_urls,
            stats,
        }
    }

    pub fn comment_out_manifest_pins(lua_text: &str) -> String {
        let lua_text = lua_text.strip_prefix('\u{feff}').unwrap_or(lua_text);
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
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let pattern = format!(
            r"(?i)^(\s*)((?:--+\s*)+)?(setManifestid\s*\(\s*{}\s*,.*)$",
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
            if let Some(gid) = fallback_manifest_id
                .map(str::trim)
                .filter(|s| Self::parse_manifest_id(s).is_some())
            {
                new_lines.push(format!("setManifestid({}, \"{}\")", depot_id, gid));
                changed = true;
            }
        }

        let mut res = new_lines.join("\n");
        if text.ends_with('\n') || (text.is_empty() && changed) {
            res.push('\n');
        }
        (res, changed)
    }

    pub fn toggle_dlc_depot_in_text(text: &str, depot_id: u32) -> (String, bool) {
        Self::toggle_dlc_depot(text, depot_id, None)
    }

    pub fn toggle_dlc_depot(
        text: &str,
        depot_id: u32,
        enabled: Option<bool>,
    ) -> (String, bool) {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let pattern = format!(
            r"(?i)^(\s*)((?:--+\s*)+)?(addappid\s*\(\s*{}(?:\s*,\s*[^)]*|\s*)\).*)",
            depot_id
        );
        let Ok(re) = Regex::new(&pattern) else {
            return (text.to_string(), false);
        };

        // Also find companion setManifestid lines for this depot
        let manifest_pattern = format!(
            r"(?i)^(\s*)((?:--+\s*)+)?(setManifestid\s*\(\s*{}\s*,.*)$",
            depot_id
        );
        let manifest_re = Regex::new(&manifest_pattern).ok();

        let mut changed = false;
        let mut new_lines: Vec<String> = Vec::new();
        for line in text.lines() {
            if let Some(cap) = re.captures(line) {
                let indent = cap.get(1).map(|m| m.as_str()).unwrap_or("");
                let is_commented = cap.get(2).is_some();
                let body = cap.get(3).map(|m| m.as_str()).unwrap_or("");

                let should_enable = match enabled {
                    Some(e) => e,
                    None => is_commented,
                };

                if should_enable && is_commented {
                    new_lines.push(format!("{}{}", indent, body));
                    changed = true;
                } else if !should_enable && !is_commented {
                    new_lines.push(format!("{}-- {}", indent, body));
                    changed = true;
                } else {
                    new_lines.push(line.to_string());
                }
            } else if let Some(ref mre) = manifest_re {
                // Also comment/uncomment companion setManifestid lines
                if let Some(cap) = mre.captures(line) {
                    let indent = cap.get(1).map(|m| m.as_str()).unwrap_or("");
                    let is_commented = cap.get(2).is_some();
                    let body = cap.get(3).map(|m| m.as_str()).unwrap_or("");

                    let should_enable = match enabled {
                        Some(e) => e,
                        None => is_commented,
                    };

                    if should_enable && is_commented {
                        new_lines.push(format!("{}{}", indent, body));
                        changed = true;
                    } else if !should_enable && !is_commented {
                        new_lines.push(format!("{}-- {}", indent, body));
                        changed = true;
                    } else {
                        new_lines.push(line.to_string());
                    }
                } else {
                    new_lines.push(line.to_string());
                }
            } else {
                new_lines.push(line.to_string());
            }
        }

        if !changed && enabled == Some(true) {
            new_lines.push(format!("addappid({})", depot_id));
            changed = true;
        }

        let mut res = new_lines.join("\n");
        if text.ends_with('\n') || (text.is_empty() && changed) {
            res.push('\n');
        }
        (res, changed)
    }

    pub fn infer_app_id_from_text(lua_text: &str) -> Option<u32> {
        let lua_text = lua_text.strip_prefix('\u{feff}').unwrap_or(lua_text);
        let expanded = expand_procedural_lua(lua_text);
        let re = infer_appid_re();
        for line in expanded.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("--") {
                continue;
            }
            if let Some(cap) = re.captures(trimmed) {
                if let Some(aid) = Self::parse_app_or_depot_id(cap.get(1)?.as_str()) {
                    if aid > 0 && aid < 2_000_000_000 {
                        return Some(aid);
                    }
                }
            }
        }
        None
    }

    pub fn default_lua_template(app_id: u32, game_name: &str) -> String {
        let safe_name: String = game_name
            .chars()
            .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
            .collect();
        let trimmed = safe_name.trim();
        let header = if trimmed.is_empty() {
            String::new()
        } else {
            format!(" -- {}", trimmed)
        };
        format!(
            "-- LuaTools stplug-in configuration for AppID {app_id}{header}\n\
             addappid({app_id}){header}\n\n\
             -- Content depot with key & optional pinned manifest:\n\
             -- addappid({next_id}, 1, \"64_CHAR_HEX_KEY\")\n\
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
            addtoken(1245620, \"secret_tok\")\n\
            addprocess(1245620, \"eldenring.exe\")\n\
            forcedenuvo(1245620)\n\
            pinApp(1245620)\n\
            setlegacycdkey(1245620, \"AAAA-BBBB-CCCC-DDDD\")\n\
            setAppTicket(1245620, \"11223344\")\n\
            setETicket(1245620, \"55667788\")\n\
            seteticketurl(\"https://ticket.example.com\")\n\
            setStat(1245620, \"76561198000000000\")\n";

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
        assert_eq!(
            parsed.process_map.get(&1245620).map(String::as_str),
            Some("eldenring.exe")
        );
        assert!(parsed.force_denuvo.contains(&1245620));
        assert!(parsed.pinned_apps.contains(&1245620));
        assert_eq!(
            parsed.legacy_cd_keys.get(&1245620).map(String::as_str),
            Some("AAAA-BBBB-CCCC-DDDD")
        );
        assert_eq!(
            parsed.app_tickets.get(&1245620).map(String::as_str),
            Some("11223344")
        );
        assert_eq!(
            parsed.e_tickets.get(&1245620).map(String::as_str),
            Some("55667788")
        );
        assert_eq!(parsed.eticket_urls, vec!["https://ticket.example.com"]);
        assert_eq!(
            parsed.stats.get(&1245620).map(String::as_str),
            Some("76561198000000000")
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

    #[test]
    fn handles_utf8_bom_multi_dash_pins_and_empty_toggle() {
        let bom_lua =
            "\u{feff}addappid(730) -- CS2\n---- setManifestid(731, \"999888777\", 1024)\n";
        let parsed = LuaFileParser::parse_text(bom_lua, 0);
        assert_eq!(parsed.base_app_id, 730);
        assert_eq!(LuaFileParser::infer_app_id_from_text(bom_lua), Some(730));
        assert_eq!(
            parsed.commented_pins.get(&731).map(String::as_str),
            Some("999888777")
        );

        let (toggled, changed) = LuaFileParser::toggle_manifest_pin_in_text(bom_lua, 731, None);
        assert!(changed);
        let reparsed = LuaFileParser::parse_text(&toggled, 730);
        assert_eq!(
            reparsed.active_pins.get(&731).map(String::as_str),
            Some("999888777")
        );

        // Empty text with no fallback must not inject a stray newline
        let (empty_out, empty_changed) = LuaFileParser::toggle_manifest_pin_in_text("", 731, None);
        assert!(!empty_changed);
        assert_eq!(empty_out, "");

        // Reject non-numeric fallback manifest IDs (prevents Lua injection)
        let (bad_out, bad_changed) =
            LuaFileParser::toggle_manifest_pin_in_text("", 731, Some("123\"); os.execute(\"id"));
        assert!(!bad_changed);
        assert_eq!(bad_out, "");

        let (good_out, good_changed) =
            LuaFileParser::toggle_manifest_pin_in_text("", 731, Some("123456789"));
        assert!(good_changed);
        assert_eq!(good_out, "setManifestid(731, \"123456789\")\n");
    }

    #[test]
    fn default_lua_template_sanitizes_newlines_in_game_name() {
        let tpl = LuaFileParser::default_lua_template(440, "Team Fortress\naddappid(999)");
        let parsed = LuaFileParser::parse_text(&tpl, 440);
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.base_app_id, 440);
    }

    #[test]
    fn procedural_lua_pre_evaluator_expands_variables_concat_and_loops() {
        let procedural_script = "\
            local app = 1245620\n\
            local depot = app + 1\n\
            local key_hi = \"0123456789abcdef0123456789abcdef\"\n\
            local key_lo = \"fedcba9876543210fedcba9876543210\"\n\
            local gid = \"88887777\" .. \"66665555444\"\n\
            local dlcs = {2778580, 2778581}\n\
            \n\
            addappid(app) -- ELDEN RING\n\
            addappid(depot, 1, key_hi .. key_lo)\n\
            setManifestid(depot, gid, 52428800000)\n\
            addtoken(app, \"tok_\" .. \"secret\")\n\
            setlegacycdkey(app, \"AAAA-\" .. \"BBBB-\" .. \"CCCC\")\n\
            setAppTicket(app, \"1122\" .. \"3344\")\n\
            setETicket(app, \"5566\" .. \"7788\")\n\
            addprocess(app, \"elden\" .. \"ring.exe\")\n\
            forcedenuvo(app)\n\
            pinApp(app)\n\
            \n\
            for _, id in ipairs(dlcs) do\n\
                addappid(id)\n\
            end\n\
            for _, id in ipairs({2778582, 2778583}) do addappid(id) end\n\
            for id = 2778590, 2778591 do addappid(id) end\n";

        let parsed = LuaFileParser::parse_text(procedural_script, 0);
        assert_eq!(parsed.base_app_id, 1245620);
        assert_eq!(
            LuaFileParser::infer_app_id_from_text(procedural_script),
            Some(1245620)
        );
        assert_eq!(parsed.depot_count(), 1);
        assert_eq!(parsed.dlc_count(), 6);
        assert_eq!(
            parsed.entries[1].key.as_deref(),
            Some("0123456789abcdef0123456789abcdeffedcba9876543210fedcba9876543210")
        );
        assert_eq!(
            parsed.active_pins.get(&1245621).map(String::as_str),
            Some("8888777766665555444")
        );
        assert_eq!(
            parsed.tokens.get(&1245620).map(String::as_str),
            Some("tok_secret")
        );
        assert_eq!(
            parsed.legacy_cd_keys.get(&1245620).map(String::as_str),
            Some("AAAA-BBBB-CCCC")
        );
        assert_eq!(
            parsed.app_tickets.get(&1245620).map(String::as_str),
            Some("11223344")
        );
        assert_eq!(
            parsed.e_tickets.get(&1245620).map(String::as_str),
            Some("55667788")
        );
        assert_eq!(
            parsed.process_map.get(&1245620).map(String::as_str),
            Some("eldenring.exe")
        );
        assert!(parsed.force_denuvo.contains(&1245620));
        assert!(parsed.pinned_apps.contains(&1245620));
    }
}
