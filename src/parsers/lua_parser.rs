//! Lua file parser & manipulator (`addappid`, `setManifestid`, `addtoken`, pin toggling).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use mlua::Lua;
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
    pub stat_overrides: BTreeMap<u32, BTreeMap<String, String>>,
    // Internal fields for dynamic Lua execution
    pub(crate) key_by_id: BTreeMap<u32, Option<String>>,
    pub(crate) sizes: BTreeMap<u32, u64>,
    pub(crate) order: Vec<u32>,
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
        Regex::new(r#"(?i)^seteticketurl\s*\(\s*["']([^"']*)["']\s*\)"#).unwrap()
    })
}

fn set_stat_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)^setStat\s*\(\s*(\d+)\s*,\s*["']?([^"'),\s]+)["']?(?:\s*,\s*["']?([^"')\s]+)["']?)?\s*\)"#).unwrap()
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

    // Never statically pre-evaluate or echo function calls such as `fetch_manifest_code(...)`,
    // `fetch_manifest_code_ex(...)`, `http_get(...)`, or `http_post(...)` (BST-12, BST-13).
    if expr.contains('(') || expr.contains(')') {
        return None;
    }

    // 4. Direct integer literal
    if let Ok(n) = expr.parse::<i128>() {
        return Some(LuaVal::Num(n));
    }

    // 5. Simple integer addition (`app + 1`)
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
        // If an argument invokes a nested callback (`fetch_manifest_code`, `http_get`, etc.),
        // leave the statement untouched so the sandboxed mlua VM evaluates it genuinely (BST-12/13).
        if arg.contains('(') || arg.contains(')') {
            return None;
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
        let raw_text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let needs_vm = has_dynamic_lua(raw_text);
        let expanded = expand_procedural_lua(raw_text);
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
        let mut stat_overrides: BTreeMap<u32, BTreeMap<String, String>> = BTreeMap::new();

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
                            if let Some(val_m) = cap.get(3) {
                                let k = s_m.as_str().to_string();
                                let v = val_m.as_str().to_string();
                                stats.insert(aid, format!("{}={}", k, v));
                                stat_overrides.entry(aid).or_default().insert(k, v);
                            } else {
                                stats.insert(aid, s_m.as_str().to_string());
                            }
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

        let result = LuaContents {
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
            stat_overrides,
            key_by_id: BTreeMap::new(),
            sizes: BTreeMap::new(),
            order: Vec::new(),
        };

        // If the script uses procedural/dynamic Lua (variables, expressions, loops, conditionals,
        // functions, stdlib calls, HTTP callbacks, or GMRC callbacks), execute the original
        // unmodified script inside the sandboxed mlua 5.4 VM (BST-01, BST-10..13).
        if needs_vm {
            if let Ok(mut vm_res) = execute_lua_script(raw_text, app_id_from_name) {
                for entry in &mut vm_res.entries {
                    if entry.comment.is_none() {
                        entry.comment = comment_by_id.get(&entry.id).cloned();
                    }
                    if entry.commented_manifest_id.is_none() {
                        entry.commented_manifest_id =
                            result.commented_pins.get(&entry.id).cloned();
                    }
                }
                let active_vm_ids: BTreeSet<u32> = vm_res.entries.iter().map(|e| e.id).collect();
                vm_res.commented_pins = result.commented_pins;
                vm_res.disabled_entries = result
                    .disabled_entries
                    .into_iter()
                    .filter(|d| !active_vm_ids.contains(&d.id))
                    .collect();
                if vm_res.base_app_id == 0 {
                    vm_res.base_app_id = vm_res
                        .entries
                        .first()
                        .map(|e| e.id)
                        .unwrap_or(app_id_from_name);
                }
                return vm_res;
            }
        }

        result
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
        if has_dynamic_lua(lua_text) {
            if let Ok(vm_res) = execute_lua_script(lua_text, 0) {
                if let Some(aid) = vm_res
                    .entries
                    .iter()
                    .map(|e| e.id)
                    .find(|&id| id > 0 && id < 2_000_000_000)
                {
                    return Some(aid);
                }
            }
        }
        None
    }

    /// Evaluates a Lua-defined `fetch_manifest_code_ex(app_id, depot_id, gid)` or
    /// `fetch_manifest_code(gid)` function inside the sandboxed VM (BST-12, BST-13).
    pub fn eval_script_manifest_code(
        script: &str,
        app_id: u32,
        depot_id: u32,
        manifest_gid: u64,
    ) -> Result<Option<u64>, String> {
        execute_lua_manifest_callback(script, app_id, depot_id, manifest_gid)
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
thread_local! {
    static ALLOW_TEST_LOOPBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn is_test_loopback_allowed() -> bool {
    ALLOW_TEST_LOOPBACK.with(|c| c.get())
}

#[cfg(not(test))]
fn is_test_loopback_allowed() -> bool {
    false
}

#[cfg(test)]
fn with_test_loopback_allowed<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    struct LoopbackGuard(bool);
    impl Drop for LoopbackGuard {
        fn drop(&mut self) {
            ALLOW_TEST_LOOPBACK.with(|c| c.set(self.0));
        }
    }
    let prev = ALLOW_TEST_LOOPBACK.with(|c| {
        let old = c.get();
        c.set(true);
        old
    });
    let _guard = LoopbackGuard(prev);
    f()
}

fn strip_lua_string_contents(code: &str) -> String {
    let bytes = code.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if (in_single || in_double) && b == b'\\' {
            i = (i + 2).min(bytes.len());
            continue;
        }
        if b == b'\'' && !in_double {
            in_single = !in_single;
            out.push('\'');
        } else if b == b'"' && !in_single {
            in_double = !in_double;
            out.push('"');
        } else if in_single || in_double {
            out.push(' ');
        } else {
            out.push(b as char);
        }
        i += 1;
    }
    out
}

fn contains_lua_word(haystack: &str, word: &str) -> bool {
    let hb = haystack.as_bytes();
    let wb = word.as_bytes();
    if wb.is_empty() || wb.len() > hb.len() {
        return false;
    }
    let is_ident_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = 0usize;
    while let Some(rel) = haystack[start..].find(word) {
        let idx = start + rel;
        let end = idx + wb.len();
        let prev_ok = idx == 0 || !is_ident_byte(hb[idx - 1]);
        let next_ok = end == hb.len() || !is_ident_byte(hb[end]);
        if prev_ok && next_ok {
            return true;
        }
        start = idx + 1;
    }
    false
}

/// Detects whether a `.lua` script contains procedural/expression/callback Lua constructs
/// that must execute inside the sandboxed `mlua` 5.4 VM (BST-01, BST-10..13).
pub fn has_dynamic_lua(text: &str) -> bool {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut in_block_comment = false;

    for raw_line in text.lines() {
        let mut slice = raw_line.trim();
        if slice.is_empty() {
            continue;
        }

        if in_block_comment {
            if let Some(end_idx) = slice.find("]]") {
                in_block_comment = false;
                slice = slice[end_idx + 2..].trim();
            } else {
                continue;
            }
        }
        while let Some(start_idx) = slice.find("--[[") {
            let before = slice[..start_idx].trim();
            let after = &slice[start_idx + 4..];
            if let Some(end_idx) = after.find("]]") {
                let tail = after[end_idx + 2..].trim();
                if before.is_empty() {
                    slice = tail;
                    continue;
                }
                if line_code_is_dynamic(before) {
                    return true;
                }
                slice = tail;
            } else {
                in_block_comment = true;
                slice = before;
                break;
            }
        }

        if slice.is_empty() || slice.starts_with("--") {
            continue;
        }
        if line_code_is_dynamic(slice) {
            return true;
        }
    }

    false
}

fn line_code_is_dynamic(line: &str) -> bool {
    let (code_part, _) = strip_inline_lua_comment(line);
    let code = code_part.trim();
    if code.is_empty() {
        return false;
    }

    let stripped = strip_lua_string_contents(code);
    let low = stripped.to_ascii_lowercase();

    if low.contains('=')
        || low.contains("..")
        || low.contains('{')
        || low.contains('}')
        || low.contains('+')
        || low.contains('*')
        || low.contains('/')
        || low.contains('%')
        || low.contains('^')
        || low.contains('#')
        || low.contains("string.")
        || low.contains("table.")
        || low.contains("math.")
        || low.contains("utf8.")
        || low.matches('(').count() > 1
    {
        return true;
    }

    for kw in [
        "local",
        "for",
        "while",
        "repeat",
        "until",
        "function",
        "if",
        "elseif",
        "else",
        "do",
        "then",
        "end",
        "return",
        "and",
        "or",
        "not",
        "in",
        "http_get",
        "http_post",
        "fetch_manifest_code",
        "fetch_manifest_code_ex",
        "pairs",
        "ipairs",
        "tonumber",
        "tostring",
        "type",
        "select",
        "next",
        "pcall",
        "xpcall",
        "assert",
        "error",
        "unpack",
        "rawget",
        "rawset",
        "setmetatable",
        "getmetatable",
    ] {
        if contains_lua_word(&low, kw) {
            return true;
        }
    }

    false
}

/// Pure IP-level SSRF blocklist check covering IPv4 and IPv6 internal/special ranges (BST-10, BST-11).
pub fn is_disallowed_lua_http_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let oct = v4.octets();
            let is_cgnat = oct[0] == 100 && (oct[1] & 0b1100_0000) == 64; // 100.64.0.0/10
            let is_this_net = oct[0] == 0; // 0.0.0.0/8
            let is_ietf_protocol = oct[0] == 192 && oct[1] == 0 && oct[2] == 0; // 192.0.0.0/24
            let is_benchmark = oct[0] == 198 && (oct[1] == 18 || oct[1] == 19); // 198.18.0.0/15
            let is_reserved = (oct[0] & 0xf0) == 240; // 240.0.0.0/4 + 255.255.255.255
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || is_cgnat
                || is_this_net
                || is_ietf_protocol
                || is_benchmark
                || is_reserved
        }
        IpAddr::V6(v6) => {
            if let Some(mapped_v4) = v6.to_ipv4_mapped() {
                return is_disallowed_lua_http_ip(&IpAddr::V4(mapped_v4));
            }
            let segs = v6.segments();
            // Deprecated IPv4-compatible `::a.b.c.d` (excluding `::` and `::1` handled below)
            if segs[0..6] == [0, 0, 0, 0, 0, 0] && !v6.is_loopback() && !v6.is_unspecified() {
                let compat_v4 = Ipv4Addr::new(
                    (segs[6] >> 8) as u8,
                    (segs[6] & 0xff) as u8,
                    (segs[7] >> 8) as u8,
                    (segs[7] & 0xff) as u8,
                );
                return is_disallowed_lua_http_ip(&IpAddr::V4(compat_v4));
            }
            // NAT64 well-known prefix `64:ff9b::/96`
            if segs[0] == 0x0064 && segs[1] == 0xff9b && segs[2..6] == [0, 0, 0, 0] {
                let nat64_v4 = Ipv4Addr::new(
                    (segs[6] >> 8) as u8,
                    (segs[6] & 0xff) as u8,
                    (segs[7] >> 8) as u8,
                    (segs[7] & 0xff) as u8,
                );
                return is_disallowed_lua_http_ip(&IpAddr::V4(nat64_v4));
            }
            let is_ula = (segs[0] & 0xfe00) == 0xfc00; // fc00::/7
            let is_link_local = (segs[0] & 0xffc0) == 0xfe80; // fe80::/10
            let is_site_local = (segs[0] & 0xffc0) == 0xfec0; // fec0::/10
            let is_doc = segs[0] == 0x2001 && segs[1] == 0x0db8; // 2001:db8::/32
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || is_ula
                || is_link_local
                || is_site_local
                || is_doc
        }
    }
}

fn check_lua_http_ip_allowed(ip: &IpAddr) -> Result<(), String> {
    if is_test_loopback_allowed() && ip.is_loopback() {
        return Ok(());
    }
    if is_disallowed_lua_http_ip(ip) {
        return Err(format!("Disallowed private/internal/loopback IP '{}'", ip));
    }
    Ok(())
}

fn is_disallowed_hostname(host_lower: &str) -> bool {
    if is_test_loopback_allowed()
        && (host_lower == "localhost" || host_lower == "127.0.0.1" || host_lower == "::1" || host_lower == "[::1]")
    {
        return false;
    }
    host_lower == "localhost"
        || host_lower.ends_with(".localhost")
        || host_lower.ends_with(".local")
        || host_lower == "0.0.0.0"
        || host_lower == "::"
        || host_lower == "[::]"
        || host_lower == "::1"
        || host_lower == "[::1]"
}

/// Connect-time DNS resolver for `ureq` that enforces the SSRF IP blocklist on every resolved
/// socket address, eliminating TOCTOU DNS-rebinding attacks (BST-10, BST-11).
#[derive(Debug, Clone, Copy, Default)]
pub struct SafeSsrfResolver;

impl ureq::Resolver for SafeSsrfResolver {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<SocketAddr>> {
        let host = if let Some(rest) = netloc.strip_prefix('[') {
            rest.split_once("]:").map(|(h, _)| h).unwrap_or(netloc)
        } else {
            netloc.rsplit_once(':').map(|(h, _)| h).unwrap_or(netloc)
        };
        let host_lower = host.trim().to_ascii_lowercase();
        if is_disallowed_hostname(&host_lower) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("Disallowed loopback/local host '{}'", host_lower),
            ));
        }

        let addrs: Vec<SocketAddr> = netloc.to_socket_addrs()?.collect();
        if addrs.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("DNS resolution returned no addresses for '{}'", netloc),
            ));
        }

        for addr in &addrs {
            if let Err(msg) = check_lua_http_ip_allowed(&addr.ip()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("{} (resolved for '{}')", msg, netloc),
                ));
            }
        }

        Ok(addrs)
    }
}

fn build_safe_lua_http_agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(timeout)
        .redirects(3)
        .resolver(SafeSsrfResolver)
        .build()
}

fn validate_lua_http_url_with_resolver<F>(
    raw_url: &str,
    resolve_fn: F,
) -> Result<url::Url, String>
where
    F: FnOnce(&str, u16) -> Result<Vec<SocketAddr>, String>,
{
    let parsed = url::Url::parse(raw_url.trim())
        .map_err(|e| format!("Invalid URL '{}': {}", raw_url, e))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(format!(
                "Disallowed URL scheme '{}' (only http/https permitted)",
                other
            ));
        }
    }
    let host_raw = parsed
        .host_str()
        .ok_or_else(|| "URL missing host".to_string())?;
    let host_lower = host_raw.to_ascii_lowercase();
    if is_disallowed_hostname(&host_lower) {
        return Err(format!("Disallowed loopback/local host '{}'", host_lower));
    }

    let clean_host = host_lower.trim_matches(|c| c == '[' || c == ']');
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| "URL missing port".to_string())?;

    if let Ok(ip) = clean_host.parse::<IpAddr>() {
        check_lua_http_ip_allowed(&ip)?;
        return Ok(parsed);
    }

    let resolved = resolve_fn(clean_host, port)?;
    if resolved.is_empty() {
        return Err(format!(
            "DNS resolution returned no addresses for host '{}'",
            clean_host
        ));
    }
    for sock in &resolved {
        check_lua_http_ip_allowed(&sock.ip())
            .map_err(|msg| format!("{} (DNS resolved for '{}')", msg, clean_host))?;
    }

    Ok(parsed)
}

/// Validates that a URL is safe for sandboxed Lua HTTP callbacks (`http`/`https` only,
/// full IPv4/IPv6 internal blocklist + DNS resolution check on all returned records).
fn validate_lua_http_url(raw_url: &str) -> Result<url::Url, String> {
    validate_lua_http_url_with_resolver(raw_url, |host, port| {
        (host, port)
            .to_socket_addrs()
            .map(|it| it.collect())
            .map_err(|e| format!("DNS resolution failed for '{}:{}': {}", host, port, e))
    })
}

fn read_bounded_response_string(resp: ureq::Response, max_bytes: usize) -> Result<(String, u16), String> {
    let status = resp.status();
    let mut reader = resp.into_reader().take(max_bytes as u64);
    let mut buf = Vec::new();
    reader
        .read_to_end(&mut buf)
        .map_err(|e| format!("Failed reading HTTP response body: {}", e))?;
    Ok((String::from_utf8_lossy(&buf).into_owned(), status))
}

/// Parses a Steam CDN `manifest_request_code` from JSON, hex (`0x...` or 16-hex), or decimal text.
pub fn parse_gmrc_response(raw: &str) -> Option<u64> {
    let trimmed = raw.trim().trim_matches('"').trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with('{') {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let extract_field = |obj: &serde_json::Value| -> Option<u64> {
                for key in ["manifest_request_code", "request_code", "code", "content", "gmrc"] {
                    if let Some(field) = obj.get(key) {
                        if let Some(n) = field.as_u64() {
                            if n > 0 {
                                return Some(n);
                            }
                        }
                        if let Some(s) = field.as_str() {
                            if let Some(parsed) = parse_gmrc_response(s) {
                                return Some(parsed);
                            }
                        }
                    }
                }
                None
            };
            if let Some(code) = extract_field(&val) {
                return Some(code);
            }
            if let Some(data) = val.get("data") {
                if let Some(code) = extract_field(data) {
                    return Some(code);
                }
            }
            return None;
        }
    }

    if let Some(hex_part) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        return u64::from_str_radix(hex_part.trim(), 16)
            .ok()
            .filter(|&c| c > 0);
    }
    if let Ok(dec) = trimmed.parse::<u64>() {
        return (dec > 0).then_some(dec);
    }
    if trimmed.len() == 16 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return u64::from_str_radix(trimmed, 16).ok().filter(|&c| c > 0);
    }

    None
}

fn expand_gmrc_url_template(
    url_tpl: &str,
    app_id: u32,
    depot_id: u32,
    manifest_gid: u64,
) -> String {
    let mut out = url_tpl.trim().to_string();
    if out.contains("{depot_id}")
        || out.contains("{manifest_gid}")
        || out.contains("{gid}")
        || out.contains("{app_id}")
    {
        out = out
            .replace("{depot_id}", &depot_id.to_string())
            .replace("{manifest_gid}", &manifest_gid.to_string())
            .replace("{gid}", &manifest_gid.to_string())
            .replace("{app_id}", &app_id.to_string());
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn query_single_gmrc_endpoint(
    raw_url: &str,
    method: &str,
    body: Option<&[u8]>,
    headers: &[(String, String)],
    timeout: Duration,
    manifest_gid: u64,
) -> Result<Option<u64>, String> {
    validate_lua_http_url(raw_url)?;
    let agent = build_safe_lua_http_agent(timeout);
    let mut req = if method.eq_ignore_ascii_case("POST") {
        agent.post(raw_url.trim())
    } else {
        agent.get(raw_url.trim())
    };
    for (k, v) in headers {
        req = req.set(k, v);
    }

    let call_res = if method.eq_ignore_ascii_case("POST") {
        req.send_bytes(body.unwrap_or(&[]))
    } else {
        req.call()
    };

    let resp = match call_res {
        Ok(r) => r,
        Err(ureq::Error::Transport(tp)) => {
            let msg = tp.to_string();
            if msg.contains("Disallowed") {
                return Err(msg);
            }
            return Ok(None);
        }
        Err(_) => return Ok(None),
    };

    let Ok((text, status)) = read_bounded_response_string(resp, 64 * 1024) else {
        return Ok(None);
    };
    if !(200..300).contains(&status) {
        return Ok(None);
    }

    match parse_gmrc_response(&text) {
        // Never echo `manifest_gid` back as the request code (BST-12, BST-13).
        Some(code) if code > 0 && (manifest_gid == 0 || code != manifest_gid) => Ok(Some(code)),
        _ => Ok(None),
    }
}

fn resolve_default_gmrc(
    app_id: u32,
    depot_id: u32,
    manifest_gid: u64,
    timeout: Duration,
) -> Result<Option<u64>, String> {
    if manifest_gid == 0 {
        return Ok(None);
    }

    if let Ok(custom_base) = std::env::var("LUATOOLS_GMRC_URL") {
        let trimmed = custom_base.trim();
        if !trimmed.is_empty() {
            let url = if trimmed.contains('{') {
                expand_gmrc_url_template(trimmed, app_id, depot_id, manifest_gid)
            } else if depot_id > 0 {
                format!("{}/{}/{}", trimmed.trim_end_matches('/'), depot_id, manifest_gid)
            } else {
                format!("{}/{}", trimmed.trim_end_matches('/'), manifest_gid)
            };
            return query_single_gmrc_endpoint(&url, "GET", None, &[], timeout, manifest_gid);
        }
    }

    let mut endpoints = Vec::with_capacity(4);
    if depot_id > 0 {
        endpoints.push(format!(
            "https://manifest.opensteamtool.com/{}/{}",
            depot_id, manifest_gid
        ));
    }
    endpoints.push(format!(
        "https://manifest.opensteamtool.com/{}",
        manifest_gid
    ));
    endpoints.push(format!("http://gmrc.wudrm.com/manifest/{}", manifest_gid));
    endpoints.push(format!(
        "https://manifest.steam.run/api/manifest/{}",
        manifest_gid
    ));

    for ep in &endpoints {
        if let Some(code) = query_single_gmrc_endpoint(ep, "GET", None, &[], timeout, manifest_gid)?
        {
            return Ok(Some(code));
        }
    }

    Ok(None)
}

fn lua_val_to_u32(v: &mlua::Value) -> Option<u32> {
    match v {
        mlua::Value::Integer(n) if *n >= 0 && *n <= u32::MAX as i64 => Some(*n as u32),
        mlua::Value::Number(n) if *n >= 0.0 && *n <= u32::MAX as f64 => Some(*n as u32),
        mlua::Value::String(st) => st.to_str().ok().and_then(|s| parse_uint32(s.trim()).ok()),
        _ => None,
    }
}

fn lua_val_to_u64(v: &mlua::Value) -> Option<u64> {
    match v {
        mlua::Value::Integer(n) => Some(*n as u64),
        mlua::Value::Number(n) if *n >= 0.0 => Some(*n as u64),
        mlua::Value::String(st) => st.to_str().ok().and_then(|s| {
            let t = s.trim();
            if let Some(hx) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                u64::from_str_radix(hx, 16).ok()
            } else {
                parse_uint64(t).ok()
            }
        }),
        _ => None,
    }
}

fn extract_lua_headers(tbl_opt: Option<mlua::Table>) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    if let Some(tbl) = tbl_opt {
        for pair in tbl.pairs::<mlua::Value, mlua::Value>().flatten() {
            let key = match pair.0 {
                mlua::Value::String(s) => s.to_str().ok().map(str::to_string),
                _ => None,
            };
            let val = match pair.1 {
                mlua::Value::String(s) => s.to_str().ok().map(str::to_string),
                mlua::Value::Integer(n) => Some(n.to_string()),
                mlua::Value::Number(n) => Some(n.to_string()),
                mlua::Value::Boolean(b) => Some(b.to_string()),
                _ => None,
            };
            if let (Some(k), Some(v)) = (key, val) {
                if !k.trim().is_empty() {
                    headers.push((k, v));
                }
            }
        }
    }
    headers
}

#[derive(Default)]
struct LuaVmCollected {
    contents: LuaContents,
    entry_os: BTreeMap<u32, String>,
    entry_arch: BTreeMap<u32, String>,
    entry_proton: BTreeMap<u32, String>,
    entry_native: BTreeSet<u32>,
}

fn register_vm_global<'lua>(
    lua: &'lua Lua,
    ci_registry: &mlua::Table<'lua>,
    names: &[&str],
    func: mlua::Function<'lua>,
) -> Result<(), String> {
    for (i, &name) in names.iter().enumerate() {
        lua.globals()
            .set(name, func.clone())
            .map_err(|e| e.to_string())?;
        if i == 0 {
            ci_registry
                .set(name.to_ascii_lowercase(), func.clone())
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn create_sandboxed_lua_vm(
    app_id: u32,
) -> Result<(Lua, Arc<Mutex<LuaVmCollected>>), String> {
    let stdlib = mlua::StdLib::TABLE
        | mlua::StdLib::STRING
        | mlua::StdLib::MATH
        | mlua::StdLib::UTF8;
    let lua = Lua::new_with(stdlib, mlua::LuaOptions::default()).map_err(|e| e.to_string())?;
    let _ = lua.set_memory_limit(16 * 1024 * 1024);

    // Strip any dangerous globals that might exist in base environment
    {
        let globals = lua.globals();
        for forbidden in [
            "os",
            "io",
            "package",
            "debug",
            "dofile",
            "loadfile",
            "load",
            "require",
            "collectgarbage",
        ] {
            let _ = globals.set(forbidden, mlua::Value::Nil);
        }
    }

    // Cap total VM instructions to prevent infinite loops
    let instr_steps = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let instr_steps_hook = instr_steps.clone();
    lua.set_hook(
        mlua::HookTriggers::new().every_nth_instruction(10_000),
        move |_, _| {
            let count = instr_steps_hook.fetch_add(10_000, std::sync::atomic::Ordering::Relaxed);
            if count >= 500_000 {
                Err(mlua::Error::RuntimeError(
                    "Lua script exceeded instruction limit (500,000)".into(),
                ))
            } else {
                Ok(())
            }
        },
    );

    let state = Arc::new(Mutex::new(LuaVmCollected::default()));
    state.lock().unwrap().contents.base_app_id = app_id;

    {
        let ci_registry = lua.create_table().map_err(|e| e.to_string())?;

        // addappid(depot_id) | addappid(depot_id, "64_hex_key") | addappid(depot_id, flags[, "64_hex_key"])
        {
            let s = state.clone();
            let func = lua
                .create_function(move |_, args: mlua::Variadic<mlua::Value>| {
                    let depot_id = args.first().and_then(lua_val_to_u32).unwrap_or(0);
                    if depot_id == 0 {
                        return Ok(());
                    }

                    // Extract optional 64-char hex key from either arg 2 (2-arg form) or arg 3 (3-arg form)
                    let mut parsed_key: Option<String> = None;
                    for arg in args.iter().skip(1) {
                        if let mlua::Value::String(st) = arg {
                            if let Ok(raw) = st.to_str() {
                                let trimmed = raw.trim();
                                if trimmed.len() == 64
                                    && trimmed.chars().all(|ch| ch.is_ascii_hexdigit())
                                {
                                    parsed_key = Some(trimmed.to_ascii_lowercase());
                                    break;
                                }
                            }
                        }
                    }

                    let mut guard = s.lock().unwrap();
                    if !guard.contents.order.contains(&depot_id) {
                        guard.contents.order.push(depot_id);
                    }
                    match guard.contents.key_by_id.get_mut(&depot_id) {
                        Some(existing) => {
                            if existing.is_none() && parsed_key.is_some() {
                                *existing = parsed_key;
                            }
                        }
                        None => {
                            guard.contents.key_by_id.insert(depot_id, parsed_key);
                        }
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            register_vm_global(&lua, &ci_registry, &["addappid", "addAppId"], func)?;
        }

        // setManifestid(depot_id, manifest_gid[, size])
        {
            let s = state.clone();
            let func = lua
                .create_function(move |_, (depot_id, gid, size): (u32, String, Option<u64>)| {
                    let mut guard = s.lock().unwrap();
                    let gid_t = gid.trim().to_string();
                    if depot_id > 0 && !gid_t.is_empty() {
                        guard.contents.active_pins.insert(depot_id, gid_t);
                        if let Some(sz) = size {
                            guard.contents.sizes.insert(depot_id, sz);
                        }
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            register_vm_global(&lua, &ci_registry, &["setManifestid", "setmanifestid"], func)?;
        }

        // addtoken(app_id, token)
        {
            let s = state.clone();
            let func = lua
                .create_function(move |_, (aid, token): (u32, String)| {
                    let mut guard = s.lock().unwrap();
                    if aid > 0 && !token.is_empty() {
                        guard.contents.tokens.insert(aid, token);
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            register_vm_global(&lua, &ci_registry, &["addtoken", "addToken"], func)?;
        }

        // setAppTicket(app_id, ticket)
        {
            let s = state.clone();
            let func = lua
                .create_function(move |_, (aid, ticket): (u32, String)| {
                    let mut guard = s.lock().unwrap();
                    if aid > 0 && !ticket.is_empty() {
                        guard.contents.app_tickets.insert(aid, ticket);
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            register_vm_global(&lua, &ci_registry, &["setAppTicket", "setappticket"], func)?;
        }

        // setETicket(app_id, ticket)
        {
            let s = state.clone();
            let func = lua
                .create_function(move |_, (aid, ticket): (u32, String)| {
                    let mut guard = s.lock().unwrap();
                    if aid > 0 && !ticket.is_empty() {
                        guard.contents.e_tickets.insert(aid, ticket);
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            register_vm_global(&lua, &ci_registry, &["setETicket", "seteticket"], func)?;
        }

        // seteticketurl(url) — empty string "" clears runtime override (BST-07)
        {
            let s = state.clone();
            let func = lua
                .create_function(move |_, url: String| {
                    let mut guard = s.lock().unwrap();
                    guard.contents.eticket_urls.push(url.trim().to_string());
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            register_vm_global(&lua, &ci_registry, &["seteticketurl", "setETicketUrl"], func)?;
        }

        // setStat(app_id, steam_id) OR setStat(app_id, stat_name, stat_value)
        {
            let s = state.clone();
            let func = lua
                .create_function(move |_, args: mlua::Variadic<mlua::Value>| {
                    let aid = args.first().and_then(lua_val_to_u32).unwrap_or(0);
                    if aid == 0 || args.len() < 2 {
                        return Ok(());
                    }
                    let val_to_string = |v: &mlua::Value| -> String {
                        match v {
                            mlua::Value::String(st) => st.to_str().unwrap_or("").to_string(),
                            mlua::Value::Integer(n) => n.to_string(),
                            mlua::Value::Number(n) => n.to_string(),
                            mlua::Value::Boolean(b) => b.to_string(),
                            _ => String::new(),
                        }
                    };
                    let mut guard = s.lock().unwrap();
                    if args.len() == 2 {
                        let steam_id = val_to_string(&args[1]);
                        if !steam_id.is_empty() {
                            guard.contents.stats.insert(aid, steam_id);
                        }
                    } else {
                        let name = val_to_string(&args[1]);
                        let value = val_to_string(&args[2]);
                        if !name.is_empty() {
                            guard
                                .contents
                                .stats
                                .insert(aid, format!("{}={}", name, value));
                            guard
                                .contents
                                .stat_overrides
                                .entry(aid)
                                .or_default()
                                .insert(name, value);
                        }
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            register_vm_global(&lua, &ci_registry, &["setStat", "setstat"], func)?;
        }

        // addprocess(app_id, process_name)
        {
            let s = state.clone();
            let func = lua
                .create_function(move |_, (aid, process): (u32, String)| {
                    let mut guard = s.lock().unwrap();
                    if aid > 0 && !process.is_empty() {
                        guard.contents.process_map.insert(aid, process);
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            register_vm_global(&lua, &ci_registry, &["addprocess", "addProcess"], func)?;
        }

        // forcedenuvo(app_id)
        {
            let s = state.clone();
            let func = lua
                .create_function(move |_, aid: u32| {
                    let mut guard = s.lock().unwrap();
                    if aid > 0 {
                        guard.contents.force_denuvo.insert(aid);
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            register_vm_global(&lua, &ci_registry, &["forcedenuvo", "forceDenuvo"], func)?;
        }

        // pinApp(app_id)
        {
            let s = state.clone();
            let func = lua
                .create_function(move |_, aid: u32| {
                    let mut guard = s.lock().unwrap();
                    if aid > 0 {
                        guard.contents.pinned_apps.insert(aid);
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            register_vm_global(&lua, &ci_registry, &["pinApp", "pinapp"], func)?;
        }

        // setlegacycdkey(app_id, key)
        {
            let s = state.clone();
            let func = lua
                .create_function(move |_, (aid, key): (u32, String)| {
                    let mut guard = s.lock().unwrap();
                    if aid > 0 && !key.is_empty() {
                        guard.contents.legacy_cd_keys.insert(aid, key);
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            register_vm_global(&lua, &ci_registry, &["setlegacycdkey", "setLegacyCdKey"], func)?;
        }

        // Platform directives: setOS, setArch, setProton, setNative
        {
            let s = state.clone();
            let func = lua
                .create_function(move |_, (id, os): (u32, String)| {
                    if id > 0 && !os.trim().is_empty() {
                        s.lock()
                            .unwrap()
                            .entry_os
                            .insert(id, os.trim().to_ascii_lowercase());
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            register_vm_global(&lua, &ci_registry, &["setOS", "setos"], func)?;
        }
        {
            let s = state.clone();
            let func = lua
                .create_function(move |_, (id, arch): (u32, String)| {
                    if id > 0 && !arch.trim().is_empty() {
                        s.lock()
                            .unwrap()
                            .entry_arch
                            .insert(id, arch.trim().to_ascii_lowercase());
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            register_vm_global(&lua, &ci_registry, &["setArch", "setarch"], func)?;
        }
        {
            let s = state.clone();
            let func = lua
                .create_function(move |_, (id, proton): (u32, String)| {
                    if id > 0 && !proton.trim().is_empty() {
                        s.lock()
                            .unwrap()
                            .entry_proton
                            .insert(id, proton.trim().to_string());
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            register_vm_global(&lua, &ci_registry, &["setProton", "setproton"], func)?;
        }
        {
            let s = state.clone();
            let func = lua
                .create_function(move |_, id: u32| {
                    if id > 0 {
                        s.lock().unwrap().entry_native.insert(id);
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            register_vm_global(&lua, &ci_registry, &["setNative", "setnative"], func)?;
        }

        // http_get(url [, headers_tbl]) -> (body_string, status_code) (BST-10)
        {
            let func = lua
                .create_function(
                    move |_, (url, headers_tbl): (String, Option<mlua::Table>)| {
                        validate_lua_http_url(&url).map_err(mlua::Error::RuntimeError)?;
                        let agent = build_safe_lua_http_agent(Duration::from_secs(10));
                        let mut req = agent.get(url.trim());
                        for (k, v) in extract_lua_headers(headers_tbl) {
                            req = req.set(&k, &v);
                        }
                        match req.call() {
                            Ok(resp) => read_bounded_response_string(resp, 256 * 1024)
                                .map_err(mlua::Error::RuntimeError),
                            Err(e) => Err(mlua::Error::RuntimeError(e.to_string())),
                        }
                    },
                )
                .map_err(|e| e.to_string())?;
            register_vm_global(&lua, &ci_registry, &["http_get"], func)?;
        }

        // http_post(url, body [, headers_tbl]) -> (body_string, status_code) (BST-11)
        {
            let func = lua
                .create_function(
                    move |_, (url, body, headers_tbl): (String, mlua::String, Option<mlua::Table>)| {
                        validate_lua_http_url(&url).map_err(mlua::Error::RuntimeError)?;
                        let agent = build_safe_lua_http_agent(Duration::from_secs(10));
                        let mut req = agent.post(url.trim());
                        for (k, v) in extract_lua_headers(headers_tbl) {
                            req = req.set(&k, &v);
                        }
                        match req.send_bytes(body.as_bytes()) {
                            Ok(resp) => read_bounded_response_string(resp, 256 * 1024)
                                .map_err(mlua::Error::RuntimeError),
                            Err(e) => Err(mlua::Error::RuntimeError(e.to_string())),
                        }
                    },
                )
                .map_err(|e| e.to_string())?;
            register_vm_global(&lua, &ci_registry, &["http_post"], func)?;
        }

        // fetch_manifest_code(depot_id, manifest_gid [, app_id]) | fetch_manifest_code(manifest_gid) (BST-12)
        // Stored in `ci_registry` (`_G` `__index` fallback) so scripts can both call it directly
        // AND define `function fetch_manifest_code(gid)` overrides cleanly detectable via `raw_get`.
        {
            let default_aid = app_id;
            let func = lua
                .create_function(move |lua_ctx, args: mlua::Variadic<mlua::Value>| {
                    let (depot_id, manifest_gid, eff_app_id) = match args.len() {
                        0 => return Ok(mlua::Value::Nil),
                        1 => (0u32, lua_val_to_u64(&args[0]).unwrap_or(0), default_aid),
                        2 => (
                            lua_val_to_u32(&args[0]).unwrap_or(0),
                            lua_val_to_u64(&args[1]).unwrap_or(0),
                            default_aid,
                        ),
                        _ => (
                            lua_val_to_u32(&args[0]).unwrap_or(0),
                            lua_val_to_u64(&args[1]).unwrap_or(0),
                            lua_val_to_u32(&args[2]).unwrap_or(default_aid),
                        ),
                    };
                    if manifest_gid == 0 {
                        return Ok(mlua::Value::Nil);
                    }
                    match resolve_default_gmrc(
                        eff_app_id,
                        depot_id,
                        manifest_gid,
                        Duration::from_secs(5),
                    ) {
                        Ok(Some(code)) => Ok(mlua::Value::String(
                            lua_ctx.create_string(&code.to_string())?,
                        )),
                        Ok(None) => Ok(mlua::Value::Nil),
                        Err(ssrf_err) => Err(mlua::Error::RuntimeError(ssrf_err)),
                    }
                })
                .map_err(|e| e.to_string())?;
            ci_registry
                .set("fetch_manifest_code", func)
                .map_err(|e| e.to_string())?;
        }

        // fetch_manifest_code_ex(url_or_table_or_appid, ...) (BST-13)
        {
            let default_aid = app_id;
            let func = lua
                .create_function(move |lua_ctx, args: mlua::Variadic<mlua::Value>| {
                    if args.is_empty() {
                        return Ok(mlua::Value::Nil);
                    }

                    // Form 1: Table options `{ url = ..., depot_id = ..., manifest_gid = ..., app_id = ..., method = ..., body = ..., headers = ..., timeout_ms = ... }`
                    if let mlua::Value::Table(ref tbl) = args[0] {
                        let depot_id = tbl
                            .get::<_, mlua::Value>("depot_id")
                            .ok()
                            .and_then(|v| lua_val_to_u32(&v))
                            .or_else(|| {
                                tbl.get::<_, mlua::Value>("depot")
                                    .ok()
                                    .and_then(|v| lua_val_to_u32(&v))
                            })
                            .unwrap_or(0);
                        let manifest_gid = tbl
                            .get::<_, mlua::Value>("manifest_gid")
                            .ok()
                            .and_then(|v| lua_val_to_u64(&v))
                            .or_else(|| {
                                tbl.get::<_, mlua::Value>("gid")
                                    .ok()
                                    .and_then(|v| lua_val_to_u64(&v))
                            })
                            .or_else(|| {
                                tbl.get::<_, mlua::Value>("manifest_id")
                                    .ok()
                                    .and_then(|v| lua_val_to_u64(&v))
                            })
                            .unwrap_or(0);
                        let eff_app_id = tbl
                            .get::<_, mlua::Value>("app_id")
                            .ok()
                            .and_then(|v| lua_val_to_u32(&v))
                            .or_else(|| {
                                tbl.get::<_, mlua::Value>("appid")
                                    .ok()
                                    .and_then(|v| lua_val_to_u32(&v))
                            })
                            .unwrap_or(default_aid);
                        let method = tbl
                            .get::<_, String>("method")
                            .unwrap_or_else(|_| "GET".to_string());
                        let body_opt = tbl.get::<_, mlua::String>("body").ok();
                        let headers =
                            extract_lua_headers(tbl.get::<_, mlua::Table>("headers").ok());
                        let timeout_ms = tbl
                            .get::<_, u64>("timeout_ms")
                            .or_else(|_| tbl.get::<_, u64>("timeout"))
                            .unwrap_or(5_000)
                            .clamp(100, 10_000);
                        let timeout = Duration::from_millis(timeout_ms);

                        let url_opt = tbl
                            .get::<_, String>("url")
                            .or_else(|_| tbl.get::<_, String>("endpoint"))
                            .ok()
                            .filter(|s| !s.trim().is_empty());

                        let res = if let Some(raw_url) = url_opt {
                            let expanded = expand_gmrc_url_template(
                                &raw_url,
                                eff_app_id,
                                depot_id,
                                manifest_gid,
                            );
                            let body_bytes = body_opt.as_ref().map(|b| b.as_bytes());
                            query_single_gmrc_endpoint(
                                &expanded,
                                &method,
                                body_bytes,
                                &headers,
                                timeout,
                                manifest_gid,
                            )
                        } else {
                            resolve_default_gmrc(eff_app_id, depot_id, manifest_gid, timeout)
                        };

                        return match res {
                            Ok(Some(code)) => Ok(mlua::Value::String(
                                lua_ctx.create_string(&code.to_string())?,
                            )),
                            Ok(None) => Ok(mlua::Value::Nil),
                            Err(ssrf_err) => Err(mlua::Error::RuntimeError(ssrf_err)),
                        };
                    }

                    // Form 2: URL string first `fetch_manifest_code_ex("https://...", [depot_id, manifest_gid, app_id, headers_tbl])`
                    if let mlua::Value::String(ref st) = args[0] {
                        if let Ok(first_str) = st.to_str() {
                            let trimmed = first_str.trim();
                            if trimmed.contains("://") {
                                let depot_id = args.get(1).and_then(lua_val_to_u32).unwrap_or(0);
                                let manifest_gid =
                                    args.get(2).and_then(lua_val_to_u64).unwrap_or(0);
                                let eff_app_id =
                                    args.get(3).and_then(lua_val_to_u32).unwrap_or(default_aid);
                                let headers_tbl = args.iter().skip(1).find_map(|v| match v {
                                    mlua::Value::Table(t) => Some(t.clone()),
                                    _ => None,
                                });
                                let headers = extract_lua_headers(headers_tbl);
                                let expanded = expand_gmrc_url_template(
                                    trimmed,
                                    eff_app_id,
                                    depot_id,
                                    manifest_gid,
                                );
                                return match query_single_gmrc_endpoint(
                                    &expanded,
                                    "GET",
                                    None,
                                    &headers,
                                    Duration::from_secs(5),
                                    manifest_gid,
                                ) {
                                    Ok(Some(code)) => Ok(mlua::Value::String(
                                        lua_ctx.create_string(&code.to_string())?,
                                    )),
                                    Ok(None) => Ok(mlua::Value::Nil),
                                    Err(ssrf_err) => Err(mlua::Error::RuntimeError(ssrf_err)),
                                };
                            }
                        }
                    }

                    // Form 3: 3-arg `(app_id, depot_id, manifest_gid [, url_or_headers])`
                    let eff_app_id = args.first().and_then(lua_val_to_u32).unwrap_or(default_aid);
                    let depot_id = args.get(1).and_then(lua_val_to_u32).unwrap_or(0);
                    let manifest_gid = args.get(2).and_then(lua_val_to_u64).unwrap_or(0);
                    if manifest_gid == 0 {
                        return Ok(mlua::Value::Nil);
                    }
                    let custom_url = args.get(3).and_then(|v| match v {
                        mlua::Value::String(s) => s.to_str().ok().map(|t| t.trim().to_string()),
                        _ => None,
                    });
                    let headers_tbl = args.iter().skip(3).find_map(|v| match v {
                        mlua::Value::Table(t) => Some(t.clone()),
                        _ => None,
                    });
                    let headers = extract_lua_headers(headers_tbl);

                    let res = if let Some(url) = custom_url.filter(|u| !u.is_empty()) {
                        let expanded =
                            expand_gmrc_url_template(&url, eff_app_id, depot_id, manifest_gid);
                        query_single_gmrc_endpoint(
                            &expanded,
                            "GET",
                            None,
                            &headers,
                            Duration::from_secs(5),
                            manifest_gid,
                        )
                    } else {
                        resolve_default_gmrc(
                            eff_app_id,
                            depot_id,
                            manifest_gid,
                            Duration::from_secs(5),
                        )
                    };

                    match res {
                        Ok(Some(code)) => Ok(mlua::Value::String(
                            lua_ctx.create_string(&code.to_string())?,
                        )),
                        Ok(None) => Ok(mlua::Value::Nil),
                        Err(ssrf_err) => Err(mlua::Error::RuntimeError(ssrf_err)),
                    }
                })
                .map_err(|e| e.to_string())?;
            ci_registry
                .set("fetch_manifest_code_ex", func)
                .map_err(|e| e.to_string())?;
        }

        // Attach case-insensitive `__index` metatable on `_G` matching BST `LuaConfig.cpp:167-184`
        lua.set_named_registry_value("__lt_ci_registry", ci_registry)
            .map_err(|e| e.to_string())?;
        let mt = lua.create_table().map_err(|e| e.to_string())?;
        let index_fn = lua
            .create_function(|lua_ctx, (_, key): (mlua::Value, mlua::Value)| {
                if let mlua::Value::String(st) = key {
                    if let Ok(s) = st.to_str() {
                        let reg: mlua::Table =
                            lua_ctx.named_registry_value("__lt_ci_registry")?;
                        return reg.get::<_, mlua::Value>(s.to_ascii_lowercase());
                    }
                }
                Ok(mlua::Value::Nil)
            })
            .map_err(|e| e.to_string())?;
        mt.set("__index", index_fn).map_err(|e| e.to_string())?;
        lua.globals().set_metatable(Some(mt));
    }

    Ok((lua, state))
}

fn execute_lua_manifest_callback(
    script: &str,
    app_id: u32,
    depot_id: u32,
    manifest_gid: u64,
) -> Result<Option<u64>, String> {
    if manifest_gid == 0 {
        return Ok(None);
    }
    let script = script.strip_prefix('\u{feff}').unwrap_or(script);
    let (lua, _state) = create_sandboxed_lua_vm(app_id)?;
    lua.load(script).exec().map_err(|e| e.to_string())?;

    let parse_ret = |val: mlua::Value| -> Option<u64> {
        let code = match val {
            mlua::Value::Integer(n) if n > 0 => Some(n as u64),
            mlua::Value::Number(n) if n > 0.0 => Some(n as u64),
            mlua::Value::String(s) => s.to_str().ok().and_then(parse_gmrc_response),
            _ => None,
        }?;
        (code > 0 && code != manifest_gid).then_some(code)
    };

    if app_id != 0 && depot_id != 0 {
        if let Ok(func_ex) = lua
            .globals()
            .raw_get::<_, mlua::Function>("fetch_manifest_code_ex")
        {
            let gid_str = lua
                .create_string(&manifest_gid.to_string())
                .map_err(|e| e.to_string())?;
            let ret: mlua::Value = func_ex
                .call((app_id, depot_id, gid_str))
                .map_err(|e| e.to_string())?;
            if let Some(code) = parse_ret(ret) {
                return Ok(Some(code));
            }
        }
    }

    if let Ok(func) = lua
        .globals()
        .raw_get::<_, mlua::Function>("fetch_manifest_code")
    {
        let gid_str = lua
            .create_string(&manifest_gid.to_string())
            .map_err(|e| e.to_string())?;
        let ret: mlua::Value = func.call(gid_str).map_err(|e| e.to_string())?;
        if let Some(code) = parse_ret(ret) {
            return Ok(Some(code));
        }
    }

    Ok(None)
}

/// Execute a dynamic Lua script in a sandboxed mlua VM (BST-01, BST-10..13).
///
/// Sandboxing:
/// - Only safe Lua standard libraries (`TABLE | STRING | MATH | UTF8`) + safe `BASE` functions
/// - `os`, `io`, `package`, `debug`, `dofile`, `loadfile`, `load`, `require`, `collectgarbage` stripped (`nil`)
/// - 16 MiB memory cap and 500,000 instruction cap to prevent infinite-loop / OOM DoS
/// - `http_get`, `http_post`, `fetch_manifest_code`, and `fetch_manifest_code_ex` enforce `http`/`https`
///   schemes, full IPv4/IPv6 + connect-time DNS-rebinding SSRF filtering (`SafeSsrfResolver`), and bounded reads
pub fn execute_lua_script(script: &str, app_id: u32) -> Result<LuaContents, String> {
    let script = script.strip_prefix('\u{feff}').unwrap_or(script);
    let (lua, state) = create_sandboxed_lua_vm(app_id)?;

    // Execute the script
    lua.load(script).exec().map_err(|e| e.to_string())?;

    // Drop the Lua VM BEFORE unwrapping `state` so all closure Arcs are released cleanly!
    drop(lua);

    let collected = Arc::try_unwrap(state)
        .map_err(|_| "Internal error: Lua VM state still referenced".to_string())?
        .into_inner()
        .map_err(|_| "Internal error: Lua VM state mutex poisoned".to_string())?;

    let mut out = collected.contents;
    out.entries = out
        .order
        .iter()
        .map(|&id| LuaEntry {
            id,
            key: out.key_by_id.get(&id).cloned().flatten(),
            manifest_id: out.active_pins.get(&id).cloned(),
            commented_manifest_id: None,
            comment: None,
            size_on_disk: out.sizes.get(&id).copied(),
            os: collected.entry_os.get(&id).cloned(),
            arch: collected.entry_arch.get(&id).cloned(),
            proton: collected.entry_proton.get(&id).cloned(),
            native: collected.entry_native.contains(&id),
        })
        .collect();
    if out.base_app_id == 0 {
        out.base_app_id = out.entries.first().map(|e| e.id).unwrap_or(app_id);
    }

    Ok(out)
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

    #[test]
    fn execute_lua_script_materializes_entries_supports_2arg_and_platform_directives() {
        let script = r#"
            addappid(1245620)
            local key1 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            local key2 = "FEDCBA9876543210FEDCBA9876543210FEDCBA9876543210FEDCBA9876543210"
            addappid(1245621, 1, key1)
            addappid(1245622, key2)
            setManifestid(1245621, "8888777766665555444", 4096)
            setOS(1245621, "linux")
            setArch(1245621, "x86_64")
            setProton(1245622, "GE-Proton9-20")
            setNative(1245621)
            setStat(1245620, "76561198000000001")
            for i = 1, 2 do
                addappid(2778580 + i)
            end
        "#;
        let res = execute_lua_script(script, 1245620).expect("execute_lua_script should succeed");
        assert_eq!(res.base_app_id, 1245620);
        assert_eq!(res.entries.len(), 5);
        assert_eq!(res.depot_count(), 2);
        assert_eq!(res.dlc_count(), 2);
        assert_eq!(
            res.entries[1].key.as_deref(),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        assert_eq!(
            res.entries[2].key.as_deref(),
            Some("fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210")
        );
        assert_eq!(res.entries[1].os.as_deref(), Some("linux"));
        assert_eq!(res.entries[1].arch.as_deref(), Some("x86_64"));
        assert!(res.entries[1].native);
        assert_eq!(res.entries[2].proton.as_deref(), Some("GE-Proton9-20"));
        assert_eq!(
            res.stats.get(&1245620).map(String::as_str),
            Some("76561198000000001")
        );
    }

    #[test]
    fn execute_lua_script_sandbox_blocks_os_io_debug_and_infinite_loops_and_ssrf() {
        // os/io/package/debug/dofile/loadfile must be nil
        let sandbox_check = r#"
            assert(os == nil, "os must be stripped")
            assert(io == nil, "io must be stripped")
            assert(package == nil, "package must be stripped")
            assert(debug == nil, "debug must be stripped")
            assert(dofile == nil, "dofile must be stripped")
            assert(loadfile == nil, "loadfile must be stripped")
            addappid(480)
        "#;
        let res = execute_lua_script(sandbox_check, 480).expect("sandbox assertions should pass");
        assert_eq!(res.entries.len(), 1);

        // Infinite loop must terminate with error via instruction hook
        let inf_loop = "while true do local x = 1 + 1 end";
        let err = execute_lua_script(inf_loop, 480).unwrap_err();
        assert!(err.contains("instruction limit"), "unexpected error: {}", err);

        // SSRF URL validation
        let ssrf = r#"http_get("http://127.0.0.1:6767/status")"#;
        let ssrf_err = execute_lua_script(ssrf, 480).unwrap_err();
        assert!(
            ssrf_err.contains("Disallowed") || ssrf_err.contains("loopback"),
            "expected SSRF rejection, got: {}",
            ssrf_err
        );

        let file_scheme = r#"http_post("file:///etc/passwd", "x")"#;
        let scheme_err = execute_lua_script(file_scheme, 480).unwrap_err();
        assert!(scheme_err.contains("Disallowed URL scheme"), "got: {}", scheme_err);
    }

    fn read_full_http_request(stream: &mut std::net::TcpStream) -> String {
        use std::io::Read as _;

        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut buf = Vec::with_capacity(2048);
        let mut chunk = [0u8; 512];
        let mut expected_total: Option<usize> = None;

        loop {
            let n = match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            buf.extend_from_slice(&chunk[..n]);

            if expected_total.is_none() {
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header_end = pos + 4;
                    let headers_text = String::from_utf8_lossy(&buf[..header_end]);
                    let content_len = headers_text
                        .lines()
                        .find_map(|line| {
                            let (k, v) = line.split_once(':')?;
                            if k.trim().eq_ignore_ascii_case("content-length") {
                                v.trim().parse::<usize>().ok()
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0);
                    expected_total = Some(header_end + content_len);
                }
            }

            if let Some(total) = expected_total {
                if buf.len() >= total {
                    break;
                }
            }
        }

        String::from_utf8_lossy(&buf).into_owned()
    }

    #[test]
    fn straight_line_lua_executes_local_stdlib_http_get_and_http_post_via_parse_text() {
        use std::io::Write as _;
        use std::net::{TcpListener, TcpStream};
        use std::sync::atomic::{AtomicBool, Ordering};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_srv = stop.clone();

        let server = std::thread::spawn(move || {
            let mut handled = 0usize;
            while handled < 2 && !stop_srv.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                if stop_srv.load(Ordering::Acquire) {
                    break;
                }
                let req_str = read_full_http_request(&mut stream);
                if req_str.is_empty() {
                    continue;
                }
                if handled == 0 {
                    assert!(req_str.starts_with("GET /key "), "got: {}", req_str);
                    assert!(req_str.to_ascii_lowercase().contains("x-lua-auth: token-get"));
                    let body = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.flush();
                } else {
                    assert!(req_str.starts_with("POST /token "), "got: {}", req_str);
                    assert!(req_str.to_ascii_lowercase().contains("x-post-hdr: yes"));
                    assert!(
                        req_str.contains("appid=480&mode=straight"),
                        "missing POST body in: {}",
                        req_str
                    );
                    let body = "8392019384729102938";
                    let resp = format!(
                        "HTTP/1.1 201 Created\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.flush();
                }
                handled += 1;
            }
        });

        // Straight-line script with NO `for`/`while`/`if`/`function` keywords — only `local`,
        // `string.upper`, `http_get`, and `http_post` (BST-01, BST-10, BST-11).
        let straight_script = format!(
            "local app = tonumber(\"480\")\n\
             local depot = app + 1\n\
             ADDAPPID(app)\n\
             local fetched_key, st1 = http_get(\"http://{addr}/key\", {{[\"X-Lua-Auth\"] = \"token-get\"}})\n\
             assert(st1 == 200)\n\
             addappid(depot, 1, string.upper(fetched_key))\n\
             local posted_tok, st2 = http_post(\"http://{addr}/token\", \"appid=\" .. tostring(app) .. \"&mode=straight\", {{[\"X-Post-Hdr\"] = \"yes\"}})\n\
             assert(st2 == 201)\n\
             addtoken(app, posted_tok)\n"
        );
        assert!(has_dynamic_lua(&straight_script));
        assert!(has_dynamic_lua(r#"http_post("https://example.com/hook", "payload")"#));
        assert!(has_dynamic_lua(r#"http_get("https://example.com/data")"#));

        let parsed = with_test_loopback_allowed(|| LuaFileParser::parse_text(&straight_script, 0));
        stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(addr);
        server.join().unwrap();

        assert_eq!(parsed.base_app_id, 480);
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(
            parsed.entries[1].key.as_deref(),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        assert_eq!(
            parsed.tokens.get(&480).map(String::as_str),
            Some("8392019384729102938")
        );
    }

    #[test]
    fn ssrf_blocklist_and_dns_rebinding_reject_ipv4_ipv6_ula_link_local_mapped_and_http_post() {
        // 1. Direct IP validator coverage across all private/special IPv4 & IPv6 ranges
        for blocked_ip in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.31.5",
            "192.168.1.10",
            "169.254.169.254",
            "100.64.10.20",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "::1",
            "::",
            "fc00::1",
            "fd12:3456:789a::1",
            "fe80::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::127.0.0.1",
        ] {
            let ip: IpAddr = blocked_ip.parse().expect(blocked_ip);
            assert!(
                is_disallowed_lua_http_ip(&ip),
                "expected '{}' to be blocked by SSRF filter",
                blocked_ip
            );
        }

        // Public global unicast IPs must be permitted
        for public_ip in ["93.184.216.34", "1.1.1.1", "2606:2800:220:1:248:1893:25c8:1946"] {
            let ip: IpAddr = public_ip.parse().expect(public_ip);
            assert!(
                !is_disallowed_lua_http_ip(&ip),
                "expected public IP '{}' to be allowed",
                public_ip
            );
        }

        // 2. DNS-rebinding protection: if ANY resolved record is internal/private, reject!
        let rebind_v4 = validate_lua_http_url_with_resolver(
            "https://rebind.attacker.example/api",
            |_h, port| {
                Ok(vec![
                    SocketAddr::new("93.184.216.34".parse().unwrap(), port),
                    SocketAddr::new("127.0.0.1".parse().unwrap(), port),
                ])
            },
        );
        assert!(rebind_v4.is_err());

        let rebind_v6_ula = validate_lua_http_url_with_resolver(
            "https://rebind6.attacker.example/api",
            |_h, port| {
                Ok(vec![
                    SocketAddr::new(
                        "2606:2800:220:1:248:1893:25c8:1946".parse().unwrap(),
                        port,
                    ),
                    SocketAddr::new("fd00::dead:beef".parse().unwrap(), port),
                ])
            },
        );
        assert!(rebind_v6_ula.is_err());

        // 3. Connect-time SafeSsrfResolver & http_post / http_get VM rejection for IPv6 & metadata IPs
        use ureq::Resolver as _;
        assert!(SafeSsrfResolver.resolve("[::1]:80").is_err());
        assert!(SafeSsrfResolver.resolve("[::ffff:127.0.0.1]:8080").is_err());
        assert!(SafeSsrfResolver.resolve("[fc00::1]:443").is_err());
        assert!(SafeSsrfResolver.resolve("[fe80::1]:80").is_err());
        assert!(SafeSsrfResolver.resolve("169.254.169.254:80").is_err());

        for bad_script in [
            r#"http_post("http://[::1]:6767/rpc", "{}")"#,
            r#"http_post("http://[::ffff:127.0.0.1]:6767/rpc", "{}")"#,
            r#"http_post("http://[fc00::1]/secret", "x")"#,
            r#"http_post("http://[fe80::1]/secret", "x")"#,
            r#"http_post("http://169.254.169.254/latest/meta-data", "x")"#,
            r#"http_get("http://100.64.0.1/internal")"#,
            r#"fetch_manifest_code_ex("http://[::ffff:10.0.0.1]/gmrc", 481, "8888777766665555444")"#,
        ] {
            let err = execute_lua_script(bad_script, 480).unwrap_err();
            assert!(
                err.contains("Disallowed"),
                "script `{}` should fail SSRF check, got: {}",
                bad_script,
                err
            );
        }
    }

    #[test]
    fn fetch_manifest_code_and_ex_parse_real_gmrc_and_never_echo_manifest_gid() {
        use std::io::Write as _;
        use std::net::{TcpListener, TcpStream};
        use std::sync::atomic::{AtomicBool, Ordering};

        // 1. Static pre-evaluator must NEVER echo manifest_gid when fetch_manifest_code(_ex) appears
        let static_gmrc = r#"setManifestid(481, fetch_manifest_code(481, "8888777766665555444"))"#;
        let expanded = expand_procedural_lua(static_gmrc);
        assert!(
            !expanded.contains(r#"setManifestid(481, "8888777766665555444")"#),
            "static pre-evaluator must never echo gid as request code: {}",
            expanded
        );
        assert!(has_dynamic_lua(static_gmrc));

        // 2. Live mock GMRC HTTP server testing real JSON, hex, and anti-echo behavior
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_srv = stop.clone();

        let server = std::thread::spawn(move || {
            let mut idx = 0usize;
            while idx < 6 && !stop_srv.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                if stop_srv.load(Ordering::Acquire) {
                    break;
                }
                let req = read_full_http_request(&mut stream);
                if req.is_empty() {
                    continue;
                }
                let body = match idx % 3 {
                    0 => {
                        assert!(req.starts_with("GET /gmrc/481/8888777766665555444 "), "got: {}", req);
                        r#"{"manifest_request_code":"9876543210123456"}"#.to_string()
                    }
                    1 => {
                        assert!(req.starts_with("POST /gmrc-table "), "got: {}", req);
                        assert!(req.contains("q=1"), "missing POST body in: {}", req);
                        r#"{"data":{"request_code":"0xDEADBEEFCAFE0001"}}"#.to_string()
                    }
                    _ => {
                        // Broken server echoing manifest_gid verbatim -> must be rejected as nil!
                        r#"{"code":"8888777766665555444"}"#.to_string()
                    }
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
                idx += 1;
            }
        });

        let expected_c2: u64 = 0xDEAD_BEEF_CAFE_0001; // 16045690984503214081
        let lua_gmrc_script = format!(
            "addappid(480)\n\
             local c1 = fetch_manifest_code_ex(\"http://{addr}/gmrc/{{depot_id}}/{{gid}}\", 481, \"8888777766665555444\", 480)\n\
             assert(c1 == \"9876543210123456\", \"unexpected c1: \" .. tostring(c1))\n\
             local c2 = fetch_manifest_code_ex({{ url = \"http://{addr}/gmrc-table\", method = \"POST\", body = \"q=1\", depot_id = 481, gid = \"8888777766665555444\" }})\n\
             assert(c2 == \"{expected_c2}\", \"unexpected c2: \" .. tostring(c2))\n\
             local echoed = fetch_manifest_code_ex(\"http://{addr}/echo\", 481, \"8888777766665555444\")\n\
             assert(echoed == nil, \"echoed manifest_gid must be rejected!\")\n\
             addtoken(480, c1)\n"
        );

        let vm_direct = with_test_loopback_allowed(|| {
            execute_lua_script(&lua_gmrc_script, 480).expect("execute_lua_script should succeed")
        });
        assert_eq!(
            vm_direct.tokens.get(&480).map(String::as_str),
            Some("9876543210123456")
        );

        let parsed = with_test_loopback_allowed(|| LuaFileParser::parse_text(&lua_gmrc_script, 480));
        stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(addr);
        server.join().unwrap();
        assert_eq!(
            parsed.tokens.get(&480).map(String::as_str),
            Some("9876543210123456")
        );

        // 3. Script-defined custom callback evaluation (BST-12 / BST-13)
        let custom_cb = r#"
            function fetch_manifest_code_ex(app_id, depot_id, gid)
                if app_id == 480 and depot_id == 481 then
                    return "5544332211009988"
                end
                return gid -- echoed fallback must be rejected
            end
        "#;
        assert_eq!(
            LuaFileParser::eval_script_manifest_code(custom_cb, 480, 481, 8888777766665555444)
                .unwrap(),
            Some(5544332211009988)
        );
        assert_eq!(
            LuaFileParser::eval_script_manifest_code(custom_cb, 730, 731, 8888777766665555444)
                .unwrap(),
            None
        );
    }
}
