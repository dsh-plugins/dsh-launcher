//! MCP server management in the DSH patch layer.
//!
//! DSH mounts one MCP server per `@deepseek-ai/dsh-mcp-client` loader row, and
//! those rows live in an `insert:` block of a patch-layer file:
//!
//! * global scope  -> `<DSH_HOME>/cordis.patch.yml`
//! * profile scope -> `<DSH_HOME>/profiles/<profile>/cordis.patch.yml`
//!
//! The patch file is a top-level YAML array of loader patch entries that may
//! also carry bundle inserts, hand-written id overrides and `!!js` scalars, so
//! saving never re-serializes the whole document: the managed MCP rows are
//! spliced out of / into the raw text (delimited by marker comments) and every
//! other line is kept byte-for-byte. Reading, in contrast, parses the document
//! so hand-written MCP rows outside the marker block are still listed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::AppState;

/// The loader module every managed MCP row loads.
const MCP_CLIENT_MODULE: &str = "@deepseek-ai/dsh-mcp-client";
/// Patch-layer filename inside a DSH_HOME / profile directory.
const PATCH_FILENAME: &str = "cordis.patch.yml";
/// Comment markers delimiting the block the launcher owns.
const BLOCK_BEGIN: &str = "# dsh-launcher mcp begin";
const BLOCK_END: &str = "# dsh-launcher mcp end";
/// `config` keys the launcher form owns; every other key is preserved as-is
/// (`toolCallTimeoutMs`, `failOnStartupError`, `reconnect`, ...).
const MANAGED_CONFIG_KEYS: [&str; 8] = [
    "serverName",
    "transport",
    "url",
    "headers",
    "command",
    "args",
    "env",
    "cwd",
];
/// `serverName` budget mirrored from dsh-mcp-client (`[A-Za-z0-9_-]{1,32}`),
/// which derives the public tool names `mcp__<serverName>__<rawName>`.
const SERVER_NAME_MAX: usize = 32;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// One ordered key/value row (headers / env), mirroring the env-override editor.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpKv {
    pub key: String,
    pub value: String,
}

/// Transport selector, serialized exactly as dsh-mcp-client's `transport`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum McpTransport {
    Stdio,
    StreamableHttp,
}

/// One editable MCP server: the loader row id plus the mcp-client config.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    /// Loader entry id in the patch layer (`mcp-<serverName>`), assigned by the
    /// backend; empty when the frontend creates a new server.
    #[serde(default)]
    pub id: String,
    /// Tool namespace: `mcp__<serverName>__<rawName>`.
    pub server_name: String,
    pub transport: McpTransport,
    /// Streamable HTTP endpoint.
    #[serde(default)]
    pub url: String,
    /// Streamable HTTP request headers.
    #[serde(default)]
    pub headers: Vec<McpKv>,
    /// stdio executable.
    #[serde(default)]
    pub command: String,
    /// stdio arguments, passed without shell interpolation.
    #[serde(default)]
    pub args: Vec<String>,
    /// stdio extra environment variables.
    #[serde(default)]
    pub env: Vec<McpKv>,
    /// stdio working directory (empty = inherit).
    #[serde(default)]
    pub cwd: String,
    /// `false` writes `disabled: true` on the loader row.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Config keys outside [`MANAGED_CONFIG_KEYS`], preserved across saves.
    #[serde(default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn default_enabled() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn home_path_of(state: &AppState, home_id: &str) -> Result<PathBuf, String> {
    state
        .config
        .lock()
        .unwrap()
        .homes
        .iter()
        .find(|h| h.id == home_id)
        .map(|h| h.path.clone())
        .ok_or_else(|| "DSH_HOME 不存在".to_string())
}

/// Patch-layer path for a scope: `None` = the DSH_HOME itself (global),
/// `Some(profile)` = that profile directory.
fn patch_path(home: &Path, profile: Option<&str>) -> Result<PathBuf, String> {
    match profile {
        None => Ok(home.join(PATCH_FILENAME)),
        Some(profile) => {
            let name = profile.trim();
            if name.is_empty() {
                return Err("Profile 名称不能为空".to_string());
            }
            if name == "." || name == ".." || name.contains('/') || name.contains('\\') {
                return Err(format!("无效的 Profile 名称: {name}"));
            }
            Ok(home.join("profiles").join(name).join(PATCH_FILENAME))
        }
    }
}

fn read_patch(path: &Path) -> Result<String, String> {
    if !path.exists() {
        return Ok(String::new());
    }
    std::fs::read_to_string(path).map_err(|e| format!("读取 {PATCH_FILENAME} 失败: {e}"))
}

fn write_patch(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    std::fs::write(path, text).map_err(|e| format!("写入 {PATCH_FILENAME} 失败: {e}"))
}

// ---------------------------------------------------------------------------
// Reading: parse MCP rows out of a patch document
// ---------------------------------------------------------------------------

fn ystr(value: &str) -> serde_yaml::Value {
    serde_yaml::Value::String(value.to_string())
}

fn field<'a>(map: &'a serde_yaml::Mapping, key: &str) -> Option<&'a serde_yaml::Value> {
    map.get(ystr(key))
}

/// Flattens a YAML scalar to the string the editor shows (non-scalars -> "").
fn scalar_string(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

fn kv_rows(value: Option<&serde_yaml::Value>) -> Vec<McpKv> {
    match value {
        Some(serde_yaml::Value::Mapping(map)) => map
            .iter()
            .map(|(k, v)| McpKv {
                key: scalar_string(k),
                value: scalar_string(v),
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn string_list(value: Option<&serde_yaml::Value>) -> Vec<String> {
    match value {
        Some(serde_yaml::Value::Sequence(items)) => items.iter().map(scalar_string).collect(),
        _ => Vec::new(),
    }
}

/// Config keys outside [`MANAGED_CONFIG_KEYS`], projected to JSON for the wire.
fn extra_config(
    config: Option<&serde_yaml::Mapping>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    let Some(config) = config else {
        return out;
    };
    for (key, value) in config.iter() {
        let serde_yaml::Value::String(key) = key else {
            continue;
        };
        if MANAGED_CONFIG_KEYS.contains(&key.as_str()) {
            continue;
        }
        if let Ok(json) = serde_json::to_value(value) {
            out.insert(key.clone(), json);
        }
    }
    out
}

fn server_from_row(row: &serde_yaml::Mapping) -> McpServer {
    let config = field(row, "config").and_then(|c| c.as_mapping());
    let get = |key: &str| config.and_then(|c| field(c, key));
    let transport = match get("transport")
        .map(scalar_string)
        .unwrap_or_default()
        .as_str()
    {
        "streamable-http" => McpTransport::StreamableHttp,
        _ => McpTransport::Stdio,
    };
    McpServer {
        id: field(row, "id").map(scalar_string).unwrap_or_default(),
        server_name: get("serverName").map(scalar_string).unwrap_or_default(),
        transport,
        url: get("url").map(scalar_string).unwrap_or_default(),
        headers: kv_rows(get("headers")),
        command: get("command").map(scalar_string).unwrap_or_default(),
        args: string_list(get("args")),
        env: kv_rows(get("env")),
        cwd: get("cwd").map(scalar_string).unwrap_or_default(),
        enabled: field(row, "disabled").and_then(|d| d.as_bool()) != Some(true),
        extra: extra_config(config),
    }
}

/// Lists every `dsh-mcp-client` row of a patch document, in file order.
pub fn parse_mcp_servers(raw: &str) -> Result<Vec<McpServer>, String> {
    let mut out = Vec::new();
    if raw.trim().is_empty() {
        return Ok(out);
    }
    let doc: serde_yaml::Value =
        serde_yaml::from_str(raw).map_err(|e| format!("解析 {PATCH_FILENAME} 失败: {e}"))?;
    let entries = match doc {
        serde_yaml::Value::Sequence(entries) => entries,
        serde_yaml::Value::Null => return Ok(out),
        _ => return Err(format!("{PATCH_FILENAME} 需为顶层 YAML 数组")),
    };
    for entry in &entries {
        let Some(entry) = entry.as_mapping() else {
            continue;
        };
        let Some(serde_yaml::Value::Sequence(rows)) = field(entry, "insert") else {
            continue;
        };
        for row in rows {
            let Some(row) = row.as_mapping() else {
                continue;
            };
            if field(row, "name").map(scalar_string).unwrap_or_default() != MCP_CLIENT_MODULE {
                continue;
            }
            out.push(server_from_row(row));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Validation (mirrored field-by-field by the settings form)
// ---------------------------------------------------------------------------

fn is_server_name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= SERVER_NAME_MAX
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// RFC 7230 header field-name token.
fn is_header_key_valid(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_\u{60}|~".contains(c))
}

/// Same rule as the instance env-override editor.
fn is_env_key_valid(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// http(s) URL with a non-empty host and no whitespace (the transport only ever
/// speaks Streamable HTTP over http/https).
fn is_http_url_valid(url: &str) -> bool {
    let rest = match url.split_once("://") {
        Some((scheme, rest)) if scheme == "http" || scheme == "https" => rest,
        _ => return false,
    };
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .trim();
    !authority.is_empty()
}

fn check_kv(rows: &[McpKv], label: &str, valid: fn(&str) -> bool) -> Result<(), String> {
    let mut seen = HashSet::new();
    for row in rows {
        if !valid(&row.key) {
            return Err(format!("非法的{label}名: {}", row.key));
        }
        if !seen.insert(row.key.clone()) {
            return Err(format!("{label}名重复: {}", row.key));
        }
    }
    Ok(())
}

/// Validates one server against the other servers of the same scope. The
/// frontend runs the same rules to render field-level errors before saving.
pub fn validate_server(server: &McpServer, others: &[McpServer]) -> Result<(), String> {
    if server.server_name.trim().is_empty() {
        return Err("请填写服务器名称".to_string());
    }
    if !is_server_name_valid(&server.server_name) {
        return Err(format!(
            "服务器名称需匹配 [A-Za-z0-9_-]，且不超过 {SERVER_NAME_MAX} 个字符"
        ));
    }
    if others.iter().any(|o| o.server_name == server.server_name) {
        return Err(format!("服务器名称「{}」已存在", server.server_name));
    }
    match server.transport {
        McpTransport::StreamableHttp => {
            if server.url.trim().is_empty() {
                return Err("请填写 URL".to_string());
            }
            if !is_http_url_valid(&server.url) {
                return Err(format!(
                    "URL 需为 http(s):// 开头的合法地址: {}",
                    server.url
                ));
            }
            check_kv(&server.headers, "请求头", is_header_key_valid)?;
        }
        McpTransport::Stdio => {
            if server.command.trim().is_empty() {
                return Err("请填写启动命令".to_string());
            }
            if server.args.iter().any(|a| a.trim().is_empty()) {
                return Err("参数不能为空".to_string());
            }
            check_kv(&server.env, "环境变量", is_env_key_valid)?;
        }
    }
    Ok(())
}

/// Drops blank rows the editor may leave behind and trims scalar fields.
fn normalize(server: &mut McpServer) {
    server.server_name = server.server_name.trim().to_string();
    server.url = server.url.trim().to_string();
    server.command = server.command.trim().to_string();
    server.cwd = server.cwd.trim().to_string();
    server.args.retain(|a| !a.trim().is_empty());
    server.headers.retain(|kv| !kv.key.trim().is_empty());
    server.env.retain(|kv| !kv.key.trim().is_empty());
    for kv in server.headers.iter_mut().chain(server.env.iter_mut()) {
        kv.key = kv.key.trim().to_string();
    }
    match server.transport {
        McpTransport::Stdio => {
            server.url.clear();
            server.headers.clear();
        }
        McpTransport::StreamableHttp => {
            server.command.clear();
            server.cwd.clear();
            server.args.clear();
            server.env.clear();
        }
    }
}

/// Stable, collision-free loader id for a server: `mcp-<serverName>`, the same
/// convention hand-written rows use, suffixed when the base id is taken.
fn stable_server_id(server_name: &str, taken: &HashSet<String>) -> String {
    let base = format!("mcp-{server_name}");
    let mut id = base.clone();
    let mut index = 2;
    while taken.contains(&id) {
        id = format!("{base}-{index}");
        index += 1;
    }
    id
}

// ---------------------------------------------------------------------------
// Writing: splice the managed rows into the raw patch text
// ---------------------------------------------------------------------------

fn indent_of(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

fn unquote(value: &str) -> &str {
    let v = value.trim();
    for quote in ['\'', '"'] {
        if v.len() >= 2 && v.starts_with(quote) && v.ends_with(quote) {
            return &v[1..v.len() - 1];
        }
    }
    v
}

/// Whether an `insert:` list item is a `dsh-mcp-client` row. Only the row's own
/// keys are inspected (the `- ` line and the lines indented one level in), so a
/// nested `name:` inside `config` cannot match.
fn is_mcp_row(lines: &[&str]) -> bool {
    let Some(first) = lines.first() else {
        return false;
    };
    let base = indent_of(first) + 2;
    for (index, line) in lines.iter().enumerate() {
        let key = if index == 0 {
            line.trim().trim_start_matches("- ").trim_start()
        } else if indent_of(line) == base {
            line.trim()
        } else {
            continue;
        };
        if let Some(rest) = key.strip_prefix("name:") {
            if unquote(rest) == MCP_CLIENT_MODULE {
                return true;
            }
        }
    }
    false
}

/// Removes every managed MCP row (plus the launcher's markers, and any
/// `insert:` key left without rows) from the raw text. All other lines are
/// preserved byte-for-byte.
pub fn strip_mcp_rows(raw: &str) -> String {
    let lines: Vec<&str> = raw.lines().collect();
    let mut dropped = vec![false; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        if trimmed == BLOCK_BEGIN || trimmed == BLOCK_END {
            dropped[i] = true;
            i += 1;
            continue;
        }
        if trimmed != "insert:" && trimmed != "- insert:" {
            i += 1;
            continue;
        }
        // Collect the list items of this `insert:` block as line ranges.
        let key_indent = indent_of(lines[i]);
        let mut items: Vec<(usize, usize)> = Vec::new();
        let mut j = i + 1;
        while j < lines.len() {
            if lines[j].trim().is_empty() {
                j += 1;
                continue;
            }
            let item_indent = indent_of(lines[j]);
            if item_indent <= key_indent || !lines[j].trim_start().starts_with("- ") {
                break;
            }
            let start = j;
            let mut end = j + 1;
            j += 1;
            while j < lines.len() {
                if lines[j].trim().is_empty() {
                    j += 1;
                    continue;
                }
                if indent_of(lines[j]) <= item_indent {
                    break;
                }
                j += 1;
                end = j;
            }
            items.push((start, end));
        }
        let mut removed = 0;
        for (start, end) in &items {
            if !is_mcp_row(&lines[*start..*end]) {
                continue;
            }
            for flag in dropped.iter_mut().take(*end).skip(*start) {
                *flag = true;
            }
            removed += 1;
        }
        // An `insert:` whose only rows were MCP rows goes away with them; a
        // bare `- insert:` row is then a whole (now empty) patch entry.
        if removed > 0 && removed == items.len() {
            dropped[i] = true;
        }
        i = j.max(i + 1);
    }

    let kept: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(index, _)| !dropped[*index])
        .map(|(_, line)| *line)
        .collect();
    let mut text = kept.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

/// Whether a patch document carries no entry (only comments / blank lines).
fn is_document_empty(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .all(|line| line.is_empty() || line.starts_with('#') || line == "[]")
}

/// One loader row for a server: `{ id, name, config }` (+ `disabled`).
fn row_value(server: &McpServer) -> Result<serde_yaml::Value, String> {
    let mut config = serde_yaml::Mapping::new();
    config.insert(ystr("serverName"), ystr(&server.server_name));
    match server.transport {
        McpTransport::Stdio => {
            config.insert(ystr("transport"), ystr("stdio"));
            config.insert(ystr("command"), ystr(&server.command));
            let args = server.args.iter().map(|a| ystr(a)).collect();
            config.insert(ystr("args"), serde_yaml::Value::Sequence(args));
            config.insert(ystr("env"), kv_mapping(&server.env));
            config.insert(ystr("cwd"), ystr(&server.cwd));
        }
        McpTransport::StreamableHttp => {
            config.insert(ystr("transport"), ystr("streamable-http"));
            config.insert(ystr("url"), ystr(&server.url));
            config.insert(ystr("headers"), kv_mapping(&server.headers));
        }
    }
    // Unmanaged keys, straight back out to the file.
    for (key, value) in &server.extra {
        if MANAGED_CONFIG_KEYS.contains(&key.as_str()) {
            continue;
        }
        let value = serde_yaml::to_value(value).map_err(|e| format!("序列化 MCP 配置失败: {e}"))?;
        config.insert(ystr(key), value);
    }
    let mut row = serde_yaml::Mapping::new();
    row.insert(ystr("id"), ystr(&server.id));
    row.insert(ystr("name"), ystr(MCP_CLIENT_MODULE));
    row.insert(ystr("config"), serde_yaml::Value::Mapping(config));
    if !server.enabled {
        row.insert(ystr("disabled"), serde_yaml::Value::Bool(true));
    }
    Ok(serde_yaml::Value::Mapping(row))
}

fn kv_mapping(rows: &[McpKv]) -> serde_yaml::Value {
    let mut map = serde_yaml::Mapping::new();
    for row in rows {
        map.insert(ystr(&row.key), ystr(&row.value));
    }
    serde_yaml::Value::Mapping(map)
}

/// The launcher-owned block: one `insert:` entry carrying every MCP row.
fn render_block(servers: &[McpServer]) -> Result<String, String> {
    let mut out = String::new();
    out.push_str(BLOCK_BEGIN);
    out.push('\n');
    out.push_str("- insert:\n");
    for server in servers {
        let text = serde_yaml::to_string(&row_value(server)?)
            .map_err(|e| format!("序列化 MCP 配置失败: {e}"))?;
        let mut first = true;
        for line in text.lines() {
            if line.trim().is_empty() || line == "---" || line == "..." {
                continue;
            }
            out.push_str(if first { "    - " } else { "      " });
            out.push_str(line);
            out.push('\n');
            first = false;
        }
    }
    out.push_str(BLOCK_END);
    out.push('\n');
    Ok(out)
}

/// Rewrites `raw` so its only MCP rows are `servers`, in order.
pub fn render_patch(raw: &str, servers: &[McpServer]) -> Result<String, String> {
    // A hand-written row may omit `id`; give it the derived `mcp-<serverName>`
    // so every row the launcher writes back stays addressable by the loader.
    let mut taken: HashSet<String> = servers
        .iter()
        .filter(|s| !s.id.trim().is_empty())
        .map(|s| s.id.clone())
        .collect();
    let mut rows: Vec<McpServer> = Vec::with_capacity(servers.len());
    for server in servers {
        let mut row = server.clone();
        if row.id.trim().is_empty() {
            row.id = stable_server_id(&row.server_name, &taken);
            taken.insert(row.id.clone());
        }
        rows.push(row);
    }

    let stripped = strip_mcp_rows(raw);
    let mut lines: Vec<&str> = stripped.lines().collect();
    if !rows.is_empty() {
        // The `[]` placeholder of an otherwise empty document cannot coexist
        // with a block sequence (that would be two YAML documents).
        lines.retain(|line| line.trim() != "[]");
    }
    while lines.last().map(|line| line.trim().is_empty()) == Some(true) {
        lines.pop();
    }
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    if rows.is_empty() {
        if is_document_empty(&out) && !out.contains("[]") {
            out.push_str("[]\n");
        }
        return Ok(out);
    }
    out.push_str(&render_block(&rows)?);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Lists the MCP servers of one scope.
#[tauri::command]
pub fn list_mcp_servers(
    state: State<'_, AppState>,
    home_id: String,
    profile: Option<String>,
) -> Result<Vec<McpServer>, String> {
    let home = home_path_of(&state, &home_id)?;
    let path = patch_path(&home, profile.as_deref())?;
    parse_mcp_servers(&read_patch(&path)?)
}

/// Creates or updates one MCP server; `original_id` names the row being edited.
/// Validation failures return before any write, so a rejected save never
/// touches the file. Resolves to the scope's servers as re-read from disk.
#[tauri::command]
pub fn save_mcp_server(
    state: State<'_, AppState>,
    home_id: String,
    profile: Option<String>,
    server: McpServer,
    original_id: Option<String>,
) -> Result<Vec<McpServer>, String> {
    let home = home_path_of(&state, &home_id)?;
    let path = patch_path(&home, profile.as_deref())?;
    let raw = read_patch(&path)?;
    let mut servers = parse_mcp_servers(&raw)?;

    let index = original_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .and_then(|id| servers.iter().position(|s| s.id == id));
    if original_id.as_deref().is_some_and(|id| !id.is_empty()) && index.is_none() {
        return Err(format!(
            "找不到要编辑的 MCP 服务器「{}」",
            original_id.unwrap_or_default()
        ));
    }
    let others: Vec<McpServer> = servers
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != index)
        .map(|(_, s)| s.clone())
        .collect();

    let mut next = server;
    normalize(&mut next);
    validate_server(&next, &others)?;

    // Carry over the unmanaged keys of the row being replaced.
    if let Some(i) = index {
        for (key, value) in &servers[i].extra {
            next.extra.entry(key.clone()).or_insert(value.clone());
        }
    }
    let taken: HashSet<String> = others.iter().map(|s| s.id.clone()).collect();
    next.id = stable_server_id(&next.server_name, &taken);

    match index {
        Some(i) => servers[i] = next,
        None => servers.push(next),
    }
    let text = render_patch(&raw, &servers)?;
    write_patch(&path, &text)?;
    crate::log_info!(
        "已保存 MCP 服务器配置: {} (scope: {})",
        path.display(),
        profile.as_deref().unwrap_or("global")
    );
    parse_mcp_servers(&text)
}

/// Removes one MCP server row from a scope; other patch entries are untouched.
#[tauri::command]
pub fn delete_mcp_server(
    state: State<'_, AppState>,
    home_id: String,
    profile: Option<String>,
    id: String,
) -> Result<Vec<McpServer>, String> {
    let home = home_path_of(&state, &home_id)?;
    let path = patch_path(&home, profile.as_deref())?;
    let raw = read_patch(&path)?;
    let mut servers = parse_mcp_servers(&raw)?;
    let before = servers.len();
    servers.retain(|s| s.id != id);
    if servers.len() == before {
        return Err(format!("MCP 服务器「{id}」不存在"));
    }
    let text = render_patch(&raw, &servers)?;
    write_patch(&path, &text)?;
    crate::log_info!("已删除 MCP 服务器 {id}: {}", path.display());
    parse_mcp_servers(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The profile patch layer shipped by a hand-configured DSH profile.
    const SAMPLE: &str = r#"# Your patch layer for this dsh profile, applied after every bundle layer:
# a top-level YAML array of loader patch entries (id-targeted config
# overrides, disables, and insert lists; `!!js` expressions allowed).
- insert:
    - id: mcp-idea
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: idea
        transport: streamable-http
        url: http://127.0.0.1:64342/stream
        headers:
          IJ_MCP_SERVER_PROJECT_PATH: C:/Git/CTNH/CTNH-Modules
    - id: mcp-codegraph
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: codegraph
        transport: stdio
        command: C:\node.exe
        args:
          - shim.js
          - serve
          - '--mcp'
        env: {}
        cwd: ''
- id: ego-browser
  disabled: true
"#;

    fn stdio_server(name: &str) -> McpServer {
        McpServer {
            id: String::new(),
            server_name: name.to_string(),
            transport: McpTransport::Stdio,
            url: String::new(),
            headers: Vec::new(),
            command: "node".to_string(),
            args: vec!["serve.js".to_string()],
            env: vec![McpKv {
                key: "TOKEN".to_string(),
                value: "1".to_string(),
            }],
            cwd: String::new(),
            enabled: true,
            extra: serde_json::Map::new(),
        }
    }

    fn http_server(name: &str) -> McpServer {
        McpServer {
            id: String::new(),
            server_name: name.to_string(),
            transport: McpTransport::StreamableHttp,
            url: "http://127.0.0.1:64342/stream".to_string(),
            headers: vec![McpKv {
                key: "X-Token".to_string(),
                value: "abc".to_string(),
            }],
            command: String::new(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: String::new(),
            enabled: true,
            extra: serde_json::Map::new(),
        }
    }

    #[test]
    fn parses_both_transports_from_the_sample_profile() {
        let servers = parse_mcp_servers(SAMPLE).unwrap();
        assert_eq!(servers.len(), 2, "{servers:?}");
        assert_eq!(servers[0].id, "mcp-idea");
        assert_eq!(servers[0].server_name, "idea");
        assert_eq!(servers[0].transport, McpTransport::StreamableHttp);
        assert_eq!(servers[0].url, "http://127.0.0.1:64342/stream");
        assert_eq!(servers[0].headers[0].key, "IJ_MCP_SERVER_PROJECT_PATH");
        assert!(servers[0].enabled);
        assert_eq!(servers[1].server_name, "codegraph");
        assert_eq!(servers[1].transport, McpTransport::Stdio);
        assert_eq!(servers[1].command, "C:\\node.exe");
        assert_eq!(servers[1].args, ["shim.js", "serve", "--mcp"]);
        assert!(servers[1].env.is_empty());
        assert_eq!(servers[1].cwd, "");
    }

    #[test]
    fn rewriting_keeps_other_patch_entries_and_the_header() {
        let servers = parse_mcp_servers(SAMPLE).unwrap();
        let out = render_patch(SAMPLE, &servers).unwrap();
        assert!(out.starts_with("# Your patch layer"), "{out}");
        assert!(out.contains("- id: ego-browser\n  disabled: true"), "{out}");
        assert_eq!(out.matches(MCP_CLIENT_MODULE).count(), 2, "{out}");
        // The rewritten document parses back to the same servers.
        let again = parse_mcp_servers(&out).unwrap();
        assert_eq!(again, servers, "{out}");
    }

    #[test]
    fn non_mcp_insert_rows_survive_a_rewrite() {
        let raw = "- insert:\n    - id: dsh-pet\n      name: 'dsh-pet'\n";
        let out = render_patch(raw, &[stdio_server("codegraph")]).unwrap();
        assert!(out.contains("- id: dsh-pet"), "{out}");
        assert!(
            out.contains("name: dsh-pet") || out.contains("name: 'dsh-pet'"),
            "{out}"
        );
        assert_eq!(parse_mcp_servers(&out).unwrap().len(), 1, "{out}");
    }

    #[test]
    fn stdio_and_http_rows_serialize_with_the_mcp_client_fields() {
        let mut stdio = stdio_server("codegraph");
        stdio.id = "mcp-codegraph".to_string();
        let mut http = http_server("idea");
        http.id = "mcp-idea".to_string();
        let out = render_patch("", &[stdio.clone(), http.clone()]).unwrap();
        assert!(
            out.starts_with("# dsh-launcher mcp begin\n- insert:\n"),
            "{out}"
        );
        assert!(out.contains("    - id: mcp-codegraph"), "{out}");
        assert!(out.contains("    - id: mcp-idea"), "{out}");
        assert!(out.contains("transport: stdio"), "{out}");
        assert!(out.contains("transport: streamable-http"), "{out}");
        assert!(out.contains("command: node"), "{out}");
        assert!(out.contains("cwd: ''"), "{out}");
        assert!(out.contains("TOKEN: '1'"), "{out}");
        assert!(out.contains("url: http://127.0.0.1:64342/stream"), "{out}");
        assert!(out.contains("X-Token: abc"), "{out}");
        assert!(out.ends_with("# dsh-launcher mcp end\n"), "{out}");
        // Field-for-field round-trip through the YAML the loader reads back.
        assert_eq!(parse_mcp_servers(&out).unwrap(), vec![stdio, http], "{out}");
        assert!(out.ends_with("# dsh-launcher mcp end\n"), "{out}");
    }

    #[test]
    fn removing_the_last_server_restores_the_empty_placeholder() {
        let with_one = render_patch("# header\n[]\n", &[stdio_server("codegraph")]).unwrap();
        assert!(!with_one.contains("[]"), "{with_one}");
        let empty = render_patch(&with_one, &[]).unwrap();
        assert_eq!(empty, "# header\n[]\n", "{empty}");
    }

    #[test]
    fn disabled_rows_round_trip_as_not_enabled() {
        let mut server = stdio_server("codegraph");
        server.enabled = false;
        let out = render_patch("", &[server]).unwrap();
        assert!(out.contains("      disabled: true"), "{out}");
        let parsed = parse_mcp_servers(&out).unwrap();
        assert!(!parsed[0].enabled);
    }

    #[test]
    fn unmanaged_config_keys_are_preserved() {
        let raw = concat!(
            "- insert:\n",
            "    - id: mcp-idea\n",
            "      name: '@deepseek-ai/dsh-mcp-client'\n",
            "      config:\n",
            "        serverName: idea\n",
            "        transport: streamable-http\n",
            "        url: http://127.0.0.1:1/stream\n",
            "        toolCallTimeoutMs: 60000\n",
            "        reconnect:\n",
            "          maxAttempts: 3\n",
        );
        let servers = parse_mcp_servers(raw).unwrap();
        assert_eq!(servers[0].extra["toolCallTimeoutMs"], 60000);
        let out = render_patch(raw, &servers).unwrap();
        assert!(out.contains("toolCallTimeoutMs: 60000"), "{out}");
        assert!(out.contains("maxAttempts: 3"), "{out}");
    }

    #[test]
    fn validation_rejects_bad_fields_and_duplicate_names() {
        let mut blank = stdio_server("codegraph");
        blank.command = "  ".to_string();
        assert!(validate_server(&blank, &[]).is_err());

        let mut bad_name = stdio_server("bad name!");
        bad_name.command = "node".to_string();
        assert!(validate_server(&bad_name, &[]).is_err());

        let mut bad_url = http_server("idea");
        bad_url.url = "127.0.0.1:64342".to_string();
        assert!(validate_server(&bad_url, &[]).is_err());
        bad_url.url = "https://example.com/mcp".to_string();
        assert!(validate_server(&bad_url, &[]).is_ok());

        let mut bad_env = stdio_server("codegraph");
        bad_env.env = vec![McpKv {
            key: "1BAD".to_string(),
            value: String::new(),
        }];
        assert!(validate_server(&bad_env, &[]).is_err());

        let mut bad_header = http_server("idea");
        bad_header.headers = vec![McpKv {
            key: "bad header".to_string(),
            value: String::new(),
        }];
        assert!(validate_server(&bad_header, &[]).is_err());

        let existing = stdio_server("codegraph");
        assert!(
            validate_server(&stdio_server("codegraph"), std::slice::from_ref(&existing)).is_err()
        );
        assert!(validate_server(&stdio_server("other"), &[existing]).is_ok());
    }

    #[test]
    fn ids_stay_unique_when_a_hand_written_row_took_the_derived_id() {
        let mut taken = HashSet::new();
        taken.insert("mcp-idea".to_string());
        assert_eq!(stable_server_id("idea", &taken), "mcp-idea-2");
        assert_eq!(stable_server_id("other", &taken), "mcp-other");
    }

    #[test]
    fn scope_maps_to_the_home_or_profile_patch_file() {
        let home = Path::new("C:\\home");
        assert_eq!(
            patch_path(home, None).unwrap(),
            home.join("cordis.patch.yml")
        );
        assert_eq!(
            patch_path(home, Some("desktop")).unwrap(),
            home.join("profiles")
                .join("desktop")
                .join("cordis.patch.yml")
        );
        assert!(patch_path(home, Some("../escape")).is_err());
        assert!(patch_path(home, Some(" ")).is_err());
    }
}
