//! Model provider management in the DSH patch layer (issue #76).
//!
//! DSH routes model requests through the `providers` dict of the
//! `@deepseek-ai/dsh-llm-pi-ai` entry inside a profile's patch layer:
//!
//! * provider routes -> `<DSH_HOME>/profiles/<profile>/cordis.patch.yml`
//!   (`config.providers`, the same spot the DSH settings UI writes)
//! * credential refs -> `<DSH_HOME>/.credentials.yaml` (`refs:` map; DSH's
//!   credential priority is launch env > refs > project `.env` > home `.env`)
//!
//! Both files are shared with the DSH settings UI and hand editors, so
//! writes never re-serialize a whole document: the managed mapping block is
//! spliced out of / into the raw text and every other byte is kept. Reads
//! parse the document. Every write takes the hash the reader saw and refuses
//! to clobber an externally modified file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::AppState;

/// The plugin whose `config.providers` dict holds the provider routes.
const PI_AI_MODULE: &str = "@deepseek-ai/dsh-llm-pi-ai";
/// Patch-layer filename inside a profile directory.
const PATCH_FILENAME: &str = "cordis.patch.yml";
/// Credential store filename inside a DSH_HOME.
const CREDENTIALS_FILENAME: &str = ".credentials.yaml";
/// Route-profile keys the launcher form owns; every other key round-trips
/// through `extra` untouched (`reasoning`, `headers`, `timeoutMs`, ...).
const MANAGED_ROUTE_KEYS: [&str; 5] = ["displayName", "apiKeyEnv", "api", "baseURL", "models"];

/// Route names that resolve to a pi-ai built-in catalog provider (endpoint,
/// protocol and model catalog inherited; from the pi-ai `providers/`
/// registry). A route keying anything else is a full custom declaration.
const CATALOG_ROUTES: &[&str] = &[
    "amazon-bedrock",
    "ant-ling",
    "anthropic",
    "azure-openai-responses",
    "baseten",
    "cerebras",
    "cloudflare-ai-gateway",
    "cloudflare-workers-ai",
    "deepseek",
    "fireworks",
    "github-copilot",
    "google",
    "google-vertex",
    "groq",
    "huggingface",
    "kimi-coding",
    "minimax",
    "minimax-cn",
    "mistral",
    "moonshotai",
    "moonshotai-cn",
    "nvidia",
    "openai",
    "openai-codex",
    "opencode",
    "opencode-go",
    "openrouter",
    "qwen-token-plan",
    "qwen-token-plan-cn",
    "qwen-token-plan-individual",
    "radius",
    "together",
    "vercel-ai-gateway",
    "xai",
    "xiaomi",
    "xiaomi-token-plan-ams",
    "xiaomi-token-plan-cn",
    "xiaomi-token-plan-sgp",
    "zai",
    "zai-coding-cn",
];

fn is_catalog_route(route: &str) -> bool {
    CATALOG_ROUTES.contains(&route)
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// One model entry of a route's `models` list.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub context_window: Option<u32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Request modalities, e.g. `["text", "image"]`.
    #[serde(default)]
    pub input: Vec<String>,
}

/// One editable provider route: the `providers` dict key plus the managed
/// profile fields. Unmanaged fields round-trip through `extra`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRoute {
    /// Dict key in `providers` — the route name.
    pub route: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub api_key_env: String,
    #[serde(default)]
    pub api: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub models: Vec<ProviderModel>,
    /// Profile keys outside [`MANAGED_ROUTE_KEYS`], preserved across saves.
    #[serde(default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
    /// Whether the route names a pi-ai built-in catalog provider
    /// (recomputed backend-side; the client flag is advisory).
    #[serde(default)]
    pub catalog: bool,
}

/// The routes of one profile plus the hash guard writes must pass back.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRouteList {
    pub routes: Vec<ProviderRoute>,
    /// sha256 of the patch file at read time; a write whose `expected_hash`
    /// no longer matches is refused instead of clobbering an external edit.
    pub hash: String,
}

/// One credential-store ref, masked for display.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialRefInfo {
    pub name: String,
    /// Masked value (`sk-a…wxyz`); the full value never leaves the backend.
    pub masked: String,
    /// The instance's env_overrides already provides this name, so the
    /// credential-store layer is shadowed (launch env wins in DSH).
    pub shadowed_by_env: bool,
}

/// Credential refs of one DSH_HOME plus the hash guard.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialRefList {
    pub refs: Vec<CredentialRefInfo>,
    pub hash: String,
}

/// One readiness check of a route, translated by the frontend via `code`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCheckItem {
    /// Stable code (`instanceEdit.providerCheck.<code>` i18n key suffix).
    pub code: String,
    /// "ok" | "warn" | "unknown".
    pub status: String,
    #[serde(default)]
    pub params: std::collections::BTreeMap<String, String>,
}

/// The readiness report of one route: the worst status of its checks.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRouteReport {
    pub route: String,
    pub status: String,
    pub checks: Vec<ProviderCheckItem>,
}

// ---------------------------------------------------------------------------
// Shared text helpers (same conventions as mcp.rs)
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

fn sha256_hex(text: &str) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(text.as_bytes()))
}

fn read_text(path: &Path) -> Result<String, String> {
    if !path.exists() {
        return Ok(String::new());
    }
    std::fs::read_to_string(path).map_err(|e| format!("读取 {} 失败: {e}", path.display()))
}

fn write_text(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    std::fs::write(path, text).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}

/// Refuses the write when the on-disk content no longer matches what the
/// reader saw (a running DSH settings UI reconciles the same files).
fn ensure_unchanged(raw: &str, expected_hash: &str) -> Result<(), String> {
    if expected_hash.is_empty() {
        return Ok(());
    }
    if sha256_hex(raw) != expected_hash {
        // The STALE_HASH prefix lets the frontend branch on a stable code
        // instead of matching this human-readable message.
        return Err(
            "STALE_HASH: 配置文件已被外部修改（可能是运行中的实例或设置界面），请重新加载后再保存"
                .to_string(),
        );
    }
    Ok(())
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
        .map(crate::wsl::home_fs_path)
        .ok_or_else(|| "DSH_HOME 不存在".to_string())
}

fn profile_patch_path(home: &Path, profile: &str) -> Result<PathBuf, String> {
    let name = profile.trim();
    if name.is_empty() {
        return Err("Profile 名称不能为空".to_string());
    }
    if name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(format!("无效的 Profile 名称: {name}"));
    }
    Ok(home.join("profiles").join(name).join(PATCH_FILENAME))
}

/// The instance's launch-time environment overrides (the top credential
/// priority layer in DSH), empty when the instance is unknown.
fn env_overrides_of(state: &AppState, instance_id: &str) -> Vec<(String, String)> {
    state
        .config
        .lock()
        .unwrap()
        .instances
        .iter()
        .find(|i| i.id == instance_id)
        .map(|i| {
            i.env_overrides
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Reading: parse provider routes out of a patch document
// ---------------------------------------------------------------------------

fn model_from_value(value: &serde_yaml::Value) -> Option<ProviderModel> {
    let map = value.as_mapping()?;
    let id = field(map, "id").map(scalar_string).unwrap_or_default();
    if id.is_empty() {
        return None;
    }
    let num = |key: &str| {
        field(map, key)
            .and_then(|v| v.as_i64())
            .and_then(|n| u32::try_from(n).ok())
    };
    Some(ProviderModel {
        id,
        name: field(map, "name").map(scalar_string).unwrap_or_default(),
        context_window: num("contextWindow"),
        max_tokens: num("maxTokens"),
        input: match field(map, "input") {
            Some(serde_yaml::Value::Sequence(items)) => items.iter().map(scalar_string).collect(),
            _ => Vec::new(),
        },
    })
}

fn route_from_value(route: &str, value: &serde_yaml::Value) -> ProviderRoute {
    let map = value.as_mapping();
    let get = |key: &str| map.and_then(|m| field(m, key));
    let mut extra = serde_json::Map::new();
    if let Some(map) = map {
        for (key, value) in map.iter() {
            let serde_yaml::Value::String(key) = key else {
                continue;
            };
            if MANAGED_ROUTE_KEYS.contains(&key.as_str()) {
                continue;
            }
            if let Ok(json) = serde_json::to_value(value) {
                extra.insert(key.clone(), json);
            }
        }
    }
    ProviderRoute {
        route: route.to_string(),
        display_name: get("displayName").map(scalar_string).unwrap_or_default(),
        api_key_env: get("apiKeyEnv").map(scalar_string).unwrap_or_default(),
        api: get("api").map(scalar_string).unwrap_or_default(),
        base_url: get("baseURL").map(scalar_string).unwrap_or_default(),
        models: match get("models") {
            Some(serde_yaml::Value::Sequence(items)) => {
                items.iter().filter_map(model_from_value).collect()
            }
            _ => Vec::new(),
        },
        extra,
        catalog: is_catalog_route(route),
    }
}

/// Lists the configured provider routes of a patch document, in file order.
pub fn parse_provider_routes(raw: &str) -> Result<Vec<ProviderRoute>, String> {
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
        if field(entry, "name").map(scalar_string).unwrap_or_default() != PI_AI_MODULE {
            continue;
        }
        let Some(providers) = field(entry, "config")
            .and_then(|c| c.as_mapping())
            .and_then(|c| field(c, "providers"))
            .and_then(|p| p.as_mapping())
        else {
            continue;
        };
        for (key, value) in providers.iter() {
            let route = scalar_string(key);
            if route.is_empty() {
                continue;
            }
            out.push(route_from_value(&route, value));
        }
        // The settings UI writes a single llm-pi-ai entry; later duplicates
        // would shadow, so only the first is the effective one.
        break;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Validation (mirrored field-by-field by the settings form)
// ---------------------------------------------------------------------------

/// Same rule as the instance env-override editor.
fn is_env_key_valid(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// New route ids: lowercase letters, digits and underscores, starting with a
/// letter and never ending on an underscore (the credential ref name is
/// derived from the id by uppercasing it). Routes created before this rule
/// (e.g. kebab-case keys the DSH settings UI wrote) are grandfathered — see
/// [`validate_route`].
fn is_route_key_valid(route: &str) -> bool {
    let bytes = route.as_bytes();
    if bytes.is_empty() || route.len() > 64 {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    if bytes[route.len() - 1] == b'_' {
        return false;
    }
    route
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// http(s) URL with a non-empty host and no whitespace.
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

/// Advisory loopback check for the insecure-endpoint warning: only a
/// literal loopback host (`localhost` or a 127/8 IPv4 address) exempts
/// `http://`. A lookalike such as `http://127.evil.com` does not parse as
/// an IPv4 address and stays insecure.
fn is_loopback_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', ':', '?', '#']).next().unwrap_or_default();
    host == "localhost"
        || host
            .parse::<std::net::Ipv4Addr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

/// Validates one route against the other routes of the same profile.
/// `original` names the route being edited: an unchanged key keeps its
/// legacy form (the DSH settings UI writes kebab-case keys, which new
/// launcher-created routes no longer accept). The frontend runs the same
/// rules to render field-level errors before saving.
pub fn validate_route(
    route: &ProviderRoute,
    others: &[ProviderRoute],
    original: Option<&str>,
) -> Result<(), String> {
    if route.route.trim().is_empty() {
        return Err("请填写路由 ID".to_string());
    }
    if original != Some(route.route.as_str()) && !is_route_key_valid(&route.route) {
        return Err(
            "路由 ID 需以小写字母开头，只能包含小写字母、数字、下划线，且不能以下划线结尾"
                .to_string(),
        );
    }
    if others.iter().any(|o| o.route == route.route) {
        return Err(format!("路由「{}」已存在", route.route));
    }
    if !route.api_key_env.is_empty() && !is_env_key_valid(&route.api_key_env) {
        return Err(format!("凭据引用需为合法环境变量名: {}", route.api_key_env));
    }
    let catalog = is_catalog_route(&route.route);
    if !route.base_url.is_empty() && !is_http_url_valid(&route.base_url) {
        return Err(format!(
            "baseURL 需为 http(s):// 开头的合法地址: {}",
            route.base_url
        ));
    }
    // Mirroring DSH's write-time assertServiceable: a route the installed
    // catalog does not ship must declare its protocol and endpoint.
    if !catalog {
        if route.api.trim().is_empty() {
            return Err("自定义路由需选择 api 协议（目录路由可省略以继承目录）".to_string());
        }
        if route.base_url.trim().is_empty() {
            return Err("自定义路由需填写 baseURL（目录路由可省略以继承目录）".to_string());
        }
    }
    let mut seen = std::collections::HashSet::new();
    for model in &route.models {
        if model.id.trim().is_empty() {
            return Err("模型 id 不能为空".to_string());
        }
        if !seen.insert(model.id.clone()) {
            return Err(format!("模型 id 重复: {}", model.id));
        }
    }
    Ok(())
}

fn normalize(route: &mut ProviderRoute) {
    route.route = route.route.trim().to_string();
    route.display_name = route.display_name.trim().to_string();
    route.api_key_env = route.api_key_env.trim().to_string();
    route.api = route.api.trim().to_string();
    route.base_url = route.base_url.trim().to_string();
    route.models.retain(|m| !m.id.trim().is_empty());
    for m in route.models.iter_mut() {
        m.id = m.id.trim().to_string();
        m.name = m.name.trim().to_string();
        m.input.retain(|i| !i.trim().is_empty());
    }
    route.catalog = is_catalog_route(&route.route);
}

// ---------------------------------------------------------------------------
// Writing: splice the managed mapping block into the raw patch text
// ---------------------------------------------------------------------------

/// One top-level patch entry's line span: `[start, end)` plus the indent of
/// its `- ` line.
struct EntrySpan {
    start: usize,
    end: usize,
    indent: usize,
}

/// Finds the top-level entry whose `name` is the pi-ai module. Only the
/// entry's own keys are inspected (the `- ` line and lines indented one
/// level in), so a nested `name:` inside `config` cannot match.
fn find_pi_ai_entry(lines: &[&str]) -> Option<EntrySpan> {
    for (i, line) in lines.iter().enumerate() {
        if !line.trim_start().starts_with("- ") {
            continue;
        }
        let indent = indent_of(line);
        let mut end = lines.len();
        for (j, l) in lines.iter().enumerate().skip(i + 1) {
            if l.trim().is_empty() {
                continue;
            }
            if indent_of(l) <= indent && l.trim_start().starts_with("- ") {
                end = j;
                break;
            }
            if indent_of(l) < indent {
                end = j;
                break;
            }
        }
        let base = indent + 2;
        for (offset, l) in lines[i..end].iter().enumerate() {
            let key = if offset == 0 {
                l.trim().trim_start_matches("- ").trim_start()
            } else if indent_of(l) == base {
                l.trim()
            } else {
                continue;
            };
            if let Some(rest) = key.strip_prefix("name:") {
                if unquote(rest) == PI_AI_MODULE {
                    return Some(EntrySpan {
                        start: i,
                        end,
                        indent,
                    });
                }
            }
        }
    }
    None
}

/// The `providers:` mapping of an entry: its line, indent, the end of the
/// mapping (exclusive) and each route sub-block's `[start, end)` span.
/// `route_indent` is the indent the existing route keys actually use
/// (hand-written files may go deeper than serde_yaml's +2); new blocks must
/// be rendered with it or the document ends up with mixed indents.
struct ProvidersSpan {
    line: usize,
    indent: usize,
    route_indent: usize,
    map_end: usize,
    routes: Vec<(String, usize, usize)>,
}

fn find_providers_span(lines: &[&str], entry: &EntrySpan) -> Option<ProvidersSpan> {
    let base = entry.indent + 2;
    // `config:` key at the entry's own level, then `providers:` one level in.
    let config_line = lines[entry.start..entry.end]
        .iter()
        .enumerate()
        .find(|(offset, l)| {
            let idx = entry.start + offset;
            let key = if idx == entry.start {
                l.trim().trim_start_matches("- ").trim_start()
            } else if indent_of(l) == base {
                l.trim()
            } else {
                return false;
            };
            key == "config:" || key.starts_with("config: ")
        });
    let (config_offset, _) = config_line?;
    let config_idx = entry.start + config_offset;
    let config_indent =
        indent_of(lines[config_idx]) + if config_idx == entry.start { 2 } else { 0 };
    for j in (config_idx + 1)..entry.end {
        let l = &lines[j];
        let trimmed = l.trim_start();
        // Blank and comment lines neither end the config mapping nor count
        // as its children (comments are free-floating in YAML).
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = indent_of(l);
        if indent <= config_indent {
            break; // left the config mapping without finding providers
        }
        let key = l.trim();
        // Only a direct child of config counts: a `providers:` key nested
        // deeper (e.g. inside a hand-written sub-mapping) must not be
        // hijacked as the route table.
        if indent == config_indent + 2 && (key == "providers:" || key.starts_with("providers: ")) {
            // Inline value (`providers: {}`) is treated as an empty mapping.
            let mut routes = Vec::new();
            let mut map_end = entry.end;
            let mut route_indent: Option<usize> = None;
            let mut k = j + 1;
            while k < entry.end {
                let l2 = &lines[k];
                let t2 = l2.trim_start();
                // Comments do not terminate a mapping in YAML and are never
                // route keys — skip them like blank lines.
                if t2.is_empty() || t2.starts_with('#') {
                    k += 1;
                    continue;
                }
                let ind2 = indent_of(l2);
                if ind2 <= indent {
                    map_end = k;
                    break;
                }
                // The first child establishes the route-key indent, so both
                // serde_yaml's +2 and hand-written deeper indents are found.
                let ri = *route_indent.get_or_insert(ind2);
                if ind2 == ri {
                    let start = k;
                    let mut end = k + 1;
                    let mut m = k + 1;
                    while m < entry.end {
                        let l3 = &lines[m];
                        if l3.trim().is_empty() {
                            m += 1;
                            continue;
                        }
                        if l3.trim_start().starts_with('#') {
                            // A comment deeper than the route key stays with
                            // the block; one at route indent or less floats
                            // between routes and belongs to neither.
                            if indent_of(l3) > ri {
                                m += 1;
                                end = m;
                            } else {
                                m += 1;
                            }
                            continue;
                        }
                        if indent_of(l3) <= ri {
                            break;
                        }
                        m += 1;
                        end = m;
                    }
                    let name = l2
                        .trim()
                        .split(':')
                        .next()
                        .map(unquote)
                        .unwrap_or_default()
                        .to_string();
                    routes.push((name, start, end));
                    k = m.max(k + 1);
                } else {
                    k += 1;
                }
            }
            return Some(ProvidersSpan {
                line: j,
                indent,
                route_indent: route_indent.unwrap_or(indent + 2),
                map_end,
                routes,
            });
        }
    }
    None
}

/// The `config:` key line of an entry (for inserting a missing `providers:`).
fn find_config_line(lines: &[&str], entry: &EntrySpan) -> Option<usize> {
    let base = entry.indent + 2;
    lines[entry.start..entry.end]
        .iter()
        .enumerate()
        .find(|(offset, l)| {
            let idx = entry.start + offset;
            let key = if idx == entry.start {
                l.trim().trim_start_matches("- ").trim_start()
            } else if indent_of(l) == base {
                l.trim()
            } else {
                return false;
            };
            key == "config:" || key.starts_with("config: ")
        })
        .map(|(offset, _)| entry.start + offset)
}

/// The `name:` key line of an entry (fallback anchor for a missing `config:`).
fn find_name_line(lines: &[&str], entry: &EntrySpan) -> Option<usize> {
    let base = entry.indent + 2;
    lines[entry.start..entry.end]
        .iter()
        .enumerate()
        .find(|(offset, l)| {
            let idx = entry.start + offset;
            let key = if idx == entry.start {
                l.trim().trim_start_matches("- ").trim_start()
            } else if indent_of(l) == base {
                l.trim()
            } else {
                return false;
            };
            key.starts_with("name:")
        })
        .map(|(offset, _)| entry.start + offset)
}

fn model_value(model: &ProviderModel) -> serde_yaml::Value {
    let mut map = serde_yaml::Mapping::new();
    map.insert(ystr("id"), ystr(&model.id));
    if !model.name.is_empty() {
        map.insert(ystr("name"), ystr(&model.name));
    }
    if let Some(cw) = model.context_window {
        map.insert(ystr("contextWindow"), serde_yaml::Value::Number(cw.into()));
    }
    if let Some(mt) = model.max_tokens {
        map.insert(ystr("maxTokens"), serde_yaml::Value::Number(mt.into()));
    }
    if !model.input.is_empty() {
        let items = model.input.iter().map(|i| ystr(i)).collect();
        map.insert(ystr("input"), serde_yaml::Value::Sequence(items));
    }
    serde_yaml::Value::Mapping(map)
}

/// The YAML sub-block of one route (`<route>:` plus its profile), every line
/// indented `indent` spaces, terminated by a newline.
fn render_route_block(route: &ProviderRoute, indent: usize) -> Result<String, String> {
    let mut profile = serde_yaml::Mapping::new();
    if !route.display_name.is_empty() {
        profile.insert(ystr("displayName"), ystr(&route.display_name));
    }
    if !route.api_key_env.is_empty() {
        profile.insert(ystr("apiKeyEnv"), ystr(&route.api_key_env));
    }
    if !route.api.is_empty() {
        profile.insert(ystr("api"), ystr(&route.api));
    }
    if !route.base_url.is_empty() {
        profile.insert(ystr("baseURL"), ystr(&route.base_url));
    }
    if !route.models.is_empty() {
        let models = route.models.iter().map(model_value).collect();
        profile.insert(ystr("models"), serde_yaml::Value::Sequence(models));
    }
    for (key, value) in &route.extra {
        if MANAGED_ROUTE_KEYS.contains(&key.as_str()) {
            continue;
        }
        let value = serde_yaml::to_value(value).map_err(|e| format!("序列化路由配置失败: {e}"))?;
        profile.insert(ystr(key), value);
    }
    let mut root = serde_yaml::Mapping::new();
    root.insert(ystr(&route.route), serde_yaml::Value::Mapping(profile));
    let text = serde_yaml::to_string(&serde_yaml::Value::Mapping(root))
        .map_err(|e| format!("序列化路由配置失败: {e}"))?;
    let pad = " ".repeat(indent);
    let mut out = String::new();
    for line in text.lines() {
        if line.trim().is_empty() || line == "---" || line == "..." {
            continue;
        }
        out.push_str(&pad);
        out.push_str(line);
        out.push('\n');
    }
    Ok(out)
}

/// Guards against splicing block children below a non-empty inline flow
/// value (e.g. `providers: {old: {...}}`): rewriting the key to its bare
/// block form would silently drop the inline entries. An empty inline map
/// (`key: {}`, the collapsed state of a just-emptied mapping) is fine — the
/// caller rewrites the key line before inserting.
fn ensure_no_inline_entries(line: &str, key: &str) -> Result<(), String> {
    // The key line may be the entry's own `- ` line (e.g. `- config: {}`).
    let t = line.trim().trim_start_matches("- ").trim_start();
    if t == format!("{key}:") {
        return Ok(());
    }
    let value = t[key.len() + 1..].trim();
    if value == "{}" {
        return Ok(());
    }
    Err(format!(
        "「{key}」为包含既有条目的内联 flow 形式，为避免数据丢失请先手工展开为块形式后再保存"
    ))
}

/// Inserts `route`'s block into the raw patch text, replacing the block of
/// `replaces` when that route already exists. Everything outside the spliced
/// region is preserved byte-for-byte.
pub fn splice_route(
    raw: &str,
    route: &ProviderRoute,
    replaces: Option<&str>,
) -> Result<String, String> {
    let lines: Vec<&str> = raw.lines().collect();
    let Some(entry) = find_pi_ai_entry(&lines) else {
        // No llm-pi-ai entry yet: append one. The `[]` placeholder of an
        // otherwise empty document cannot coexist with a block sequence.
        let mut kept: Vec<&str> = lines.to_vec();
        kept.retain(|line| line.trim() != "[]");
        while kept.last().map(|line| line.trim().is_empty()) == Some(true) {
            kept.pop();
        }
        let mut out = kept.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("- id: llm-pi-ai\n");
        out.push_str(&format!("  name: '{PI_AI_MODULE}'\n"));
        out.push_str("  config:\n");
        out.push_str("    providers:\n");
        out.push_str(&render_route_block(route, 6)?);
        return Ok(out);
    };

    match find_providers_span(&lines, &entry) {
        Some(span) => {
            // Render with the indent the existing route keys actually use,
            // or a hand-written deeper-indented file gets mixed-indent
            // children appended (invalid YAML).
            let block = render_route_block(route, span.route_indent)?;
            let target = replaces.unwrap_or(&route.route);
            let mut out_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
            if let Some((_, start, end)) = span.routes.iter().find(|(name, _, _)| name == target) {
                // Replace the existing sub-block.
                let mut new_lines: Vec<String> = out_lines[..*start].to_vec();
                new_lines.extend(block.trim_end_matches('\n').split('\n').map(String::from));
                new_lines.extend(out_lines[*end..].iter().cloned());
                out_lines = new_lines;
            } else {
                // Append at the end of the providers mapping.
                let insert_at = span.map_end;
                let block_lines: Vec<String> = block
                    .trim_end_matches('\n')
                    .split('\n')
                    .map(String::from)
                    .collect();
                // An inline value cannot have children appended below it:
                // collapse an empty one (`providers: {}`) to the bare block
                // form first, refuse a non-empty flow value.
                ensure_no_inline_entries(&out_lines[span.line], "providers")?;
                if out_lines[span.line].trim() != "providers:" {
                    let pad = " ".repeat(span.indent);
                    out_lines[span.line] = format!("{pad}providers:");
                }
                let mut new_lines: Vec<String> = out_lines[..insert_at].to_vec();
                new_lines.extend(block_lines);
                new_lines.extend(out_lines[insert_at..].iter().cloned());
                out_lines = new_lines;
            }
            let mut out = out_lines.join("\n");
            if raw.ends_with('\n') {
                out.push('\n');
            }
            Ok(out)
        }
        None => {
            // Entry exists without a `providers:` key: add it under config.
            let block = render_route_block(route, entry.indent + 6)?;
            let mut out_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
            let pad2 = " ".repeat(entry.indent + 2);
            let pad4 = " ".repeat(entry.indent + 4);
            let insert_at = match find_config_line(&lines, &entry) {
                Some(config_idx) => {
                    // Same inline-flow rule as `providers:` above: collapse
                    // `config: {}` to the bare block form, refuse non-empty.
                    // The key may sit on the entry's own `- ` line; the
                    // rewrite must keep that list marker.
                    ensure_no_inline_entries(&out_lines[config_idx], "config")?;
                    let t = out_lines[config_idx].trim().to_string();
                    if t != "config:" && t != "- config:" {
                        let pad = " ".repeat(indent_of(&out_lines[config_idx]));
                        let dash = if t.starts_with("- ") { "- " } else { "" };
                        out_lines[config_idx] = format!("{pad}{dash}config:");
                    }
                    out_lines.insert(config_idx + 1, format!("{pad4}providers:"));
                    config_idx + 2
                }
                None => {
                    let anchor = find_name_line(&lines, &entry).unwrap_or(entry.start);
                    out_lines.insert(anchor + 1, format!("{pad2}config:"));
                    out_lines.insert(anchor + 2, format!("{pad4}providers:"));
                    anchor + 3
                }
            };
            let block_lines: Vec<String> = block
                .trim_end_matches('\n')
                .split('\n')
                .map(String::from)
                .collect();
            for (i, l) in block_lines.iter().enumerate() {
                out_lines.insert(insert_at + i, l.clone());
            }
            let mut out = out_lines.join("\n");
            if raw.ends_with('\n') {
                out.push('\n');
            }
            Ok(out)
        }
    }
}

/// Removes one route's sub-block. A providers mapping left without routes
/// collapses to `providers: {}` so the document stays valid.
pub fn splice_route_removal(raw: &str, route: &str) -> Result<String, String> {
    let lines: Vec<&str> = raw.lines().collect();
    let Some(entry) = find_pi_ai_entry(&lines) else {
        return Err(format!("路由「{route}」不存在"));
    };
    let Some(span) = find_providers_span(&lines, &entry) else {
        return Err(format!("路由「{route}」不存在"));
    };
    let Some((_, start, end)) = span.routes.iter().find(|(name, _, _)| name == route) else {
        return Err(format!("路由「{route}」不存在"));
    };
    let mut out_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    if span.routes.len() == 1 {
        // Last route: collapse the mapping to an inline empty dict.
        let pad = " ".repeat(span.indent);
        let mut new_lines: Vec<String> = out_lines[..span.line].to_vec();
        new_lines.push(format!("{pad}providers: {{}}"));
        new_lines.extend(out_lines[*end..].iter().cloned());
        out_lines = new_lines;
    } else {
        let mut new_lines: Vec<String> = out_lines[..*start].to_vec();
        new_lines.extend(out_lines[*end..].iter().cloned());
        out_lines = new_lines;
    }
    let mut out = out_lines.join("\n");
    if raw.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Credential refs (.credentials.yaml `refs:` map)
// ---------------------------------------------------------------------------

/// Parses the `refs` map of a credentials document: name -> full value.
fn parse_credential_refs(raw: &str) -> Result<Vec<(String, String)>, String> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let doc: serde_yaml::Value =
        serde_yaml::from_str(raw).map_err(|e| format!("解析 {CREDENTIALS_FILENAME} 失败: {e}"))?;
    let serde_yaml::Value::Mapping(root) = doc else {
        return Ok(Vec::new());
    };
    let Some(refs) = field(&root, "refs").and_then(|r| r.as_mapping()) else {
        return Ok(Vec::new());
    };
    Ok(refs
        .iter()
        .map(|(k, v)| (scalar_string(k), scalar_string(v)))
        .filter(|(k, _)| !k.is_empty())
        .collect())
}

/// Masks a secret for display: the first 4 and last 4 chars survive, the
/// middle never leaves the backend. Short values mask completely.
fn mask_secret(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 8 {
        // A fixed-width mask so the length of a short secret stays secret.
        return "*".repeat(8);
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

/// Serializes a plain scalar for a `key: value` line.
fn yaml_scalar(value: &str) -> Result<String, String> {
    let text = serde_yaml::to_string(&serde_yaml::Value::String(value.to_string()))
        .map_err(|e| format!("序列化凭据失败: {e}"))?;
    Ok(text
        .trim_end_matches('\n')
        .trim_start_matches("--- ")
        .to_string())
}

/// The top-level `refs:` mapping of a credentials document: its line, the
/// end of the mapping (exclusive) and each entry's line index.
struct RefsSpan {
    line: usize,
    map_end: usize,
    entries: Vec<(String, usize)>,
}

fn find_refs_span(lines: &[&str]) -> Option<RefsSpan> {
    for (i, line) in lines.iter().enumerate() {
        if indent_of(line) != 0 {
            continue;
        }
        let key = line.trim();
        if key != "refs:" && !key.starts_with("refs: ") {
            continue;
        }
        let mut entries = Vec::new();
        let mut map_end = lines.len();
        for (j, l) in lines.iter().enumerate().skip(i + 1) {
            let trimmed = l.trim_start();
            // Comments do not terminate a mapping in YAML — skip them like
            // blank lines instead of breaking or recording phantom entries.
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if indent_of(l) == 0 {
                map_end = j;
                break;
            }
            let name = l
                .trim()
                .split(':')
                .next()
                .map(unquote)
                .unwrap_or_default()
                .to_string();
            entries.push((name, j));
        }
        return Some(RefsSpan {
            line: i,
            map_end,
            entries,
        });
    }
    None
}

/// Inserts or replaces one `refs` entry, preserving the rest of the
/// document (`version:`, `records:`, comments) byte-for-byte.
pub fn splice_credential_ref(raw: &str, name: &str, value: &str) -> Result<String, String> {
    let lines: Vec<&str> = raw.lines().collect();
    let entry_line = format!("  {name}: {}", yaml_scalar(value)?);
    match find_refs_span(&lines) {
        Some(span) => {
            let mut out_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
            if let Some((_, idx)) = span.entries.iter().find(|(n, _)| n == name) {
                out_lines[*idx] = entry_line;
            } else {
                // An inline value cannot have entries appended below it:
                // collapse an empty one (`refs: {}`) to the bare block form
                // first, refuse a non-empty flow value.
                ensure_no_inline_entries(&out_lines[span.line], "refs")?;
                if out_lines[span.line].trim() != "refs:" {
                    out_lines[span.line] = "refs:".to_string();
                }
                out_lines.insert(span.map_end, entry_line);
            }
            let mut out = out_lines.join("\n");
            if raw.ends_with('\n') {
                out.push('\n');
            }
            Ok(out)
        }
        None => {
            let had_trailing_newline = raw.ends_with('\n');
            let mut out = raw.trim_end_matches('\n').to_string();
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("refs:\n");
            out.push_str(&entry_line);
            if had_trailing_newline {
                out.push('\n');
            }
            Ok(out)
        }
    }
}

/// Removes one `refs` entry; an emptied map collapses to `refs: {}`.
pub fn splice_credential_ref_removal(raw: &str, name: &str) -> Result<String, String> {
    let lines: Vec<&str> = raw.lines().collect();
    let Some(span) = find_refs_span(&lines) else {
        return Err(format!("凭据引用「{name}」不存在"));
    };
    let Some((_, idx)) = span.entries.iter().find(|(n, _)| n == name) else {
        return Err(format!("凭据引用「{name}」不存在"));
    };
    let mut out_lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    if span.entries.len() == 1 {
        out_lines[span.line] = "refs: {}".to_string();
        out_lines.remove(*idx);
    } else {
        out_lines.remove(*idx);
    }
    let mut out = out_lines.join("\n");
    if raw.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Commands: provider routes
// ---------------------------------------------------------------------------

/// Lists the provider routes of one profile's patch layer.
#[tauri::command]
pub fn list_provider_routes(
    state: State<'_, AppState>,
    home_id: String,
    profile: String,
) -> Result<ProviderRouteList, String> {
    let home = home_path_of(&state, &home_id)?;
    let path = profile_patch_path(&home, &profile)?;
    let raw = read_text(&path)?;
    let routes = parse_provider_routes(&raw)?;
    Ok(ProviderRouteList {
        routes,
        hash: sha256_hex(&raw),
    })
}

/// Creates or updates one provider route; `original_route` names the route
/// being edited (a rename deletes the old block). Validation failures return
/// before any write. Resolves to the routes as re-read from the written text.
#[tauri::command]
pub fn save_provider_route(
    state: State<'_, AppState>,
    home_id: String,
    profile: String,
    route: ProviderRoute,
    original_route: Option<String>,
    expected_hash: String,
) -> Result<ProviderRouteList, String> {
    let home = home_path_of(&state, &home_id)?;
    let path = profile_patch_path(&home, &profile)?;
    let raw = read_text(&path)?;
    ensure_unchanged(&raw, &expected_hash)?;
    let routes = parse_provider_routes(&raw)?;

    let original = original_route.as_deref().filter(|r| !r.trim().is_empty());
    if let Some(orig) = original {
        if !routes.iter().any(|r| r.route == orig) {
            return Err(format!("找不到要编辑的路由「{orig}」"));
        }
    }
    let others: Vec<ProviderRoute> = routes
        .iter()
        .filter(|r| Some(r.route.as_str()) != original)
        .cloned()
        .collect();

    let mut next = route;
    normalize(&mut next);
    validate_route(&next, &others, original)?;

    // Carry over the unmanaged keys of the route being replaced.
    if let Some(orig) = original {
        if let Some(old) = routes.iter().find(|r| r.route == orig) {
            for (key, value) in &old.extra {
                next.extra
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
            }
        }
    }

    // A rename removes the old block first; an in-place edit replaces it.
    let text = match original {
        Some(orig) if orig != next.route => {
            let removed = splice_route_removal(&raw, orig)?;
            splice_route(&removed, &next, None)?
        }
        _ => splice_route(&raw, &next, original)?,
    };
    // Refuse to persist a document that no longer parses — defense in depth
    // for the line-based splice engine against exotic hand-written shapes.
    let reparsed = parse_provider_routes(&text)?;
    write_text(&path, &text)?;
    crate::log_info!("已保存模型供应商路由「{}」: {}", next.route, path.display());
    Ok(ProviderRouteList {
        routes: reparsed,
        hash: sha256_hex(&text),
    })
}

/// Removes one provider route; other patch entries are untouched.
#[tauri::command]
pub fn delete_provider_route(
    state: State<'_, AppState>,
    home_id: String,
    profile: String,
    route: String,
    expected_hash: String,
) -> Result<ProviderRouteList, String> {
    let home = home_path_of(&state, &home_id)?;
    let path = profile_patch_path(&home, &profile)?;
    let raw = read_text(&path)?;
    ensure_unchanged(&raw, &expected_hash)?;
    let text = splice_route_removal(&raw, &route)?;
    // Refuse to persist a document that no longer parses.
    let reparsed = parse_provider_routes(&text)?;
    write_text(&path, &text)?;
    crate::log_info!("已删除模型供应商路由 {route}: {}", path.display());
    Ok(ProviderRouteList {
        routes: reparsed,
        hash: sha256_hex(&text),
    })
}

// ---------------------------------------------------------------------------
// Commands: credential refs
// ---------------------------------------------------------------------------

/// Lists the credential refs of one DSH_HOME, masked; `instance_id` resolves
/// which refs are shadowed by the instance's launch environment.
#[tauri::command]
pub fn list_credential_refs(
    state: State<'_, AppState>,
    home_id: String,
    instance_id: String,
) -> Result<CredentialRefList, String> {
    let home = home_path_of(&state, &home_id)?;
    let path = home.join(CREDENTIALS_FILENAME);
    let raw = read_text(&path)?;
    let env = env_overrides_of(&state, &instance_id);
    let refs = parse_credential_refs(&raw)?
        .into_iter()
        .map(|(name, value)| CredentialRefInfo {
            shadowed_by_env: env.iter().any(|(k, _)| *k == name),
            masked: mask_secret(&value),
            name,
        })
        .collect();
    Ok(CredentialRefList {
        refs,
        hash: sha256_hex(&raw),
    })
}

/// Writes one credential ref. Refused when the instance's launch environment
/// already provides the name: DSH resolves launch env first, so the write
/// would silently never take effect.
#[tauri::command]
pub fn set_credential_ref(
    state: State<'_, AppState>,
    home_id: String,
    instance_id: String,
    name: String,
    value: String,
    expected_hash: String,
) -> Result<CredentialRefList, String> {
    let name = name.trim().to_string();
    if !is_env_key_valid(&name) {
        return Err(format!("凭据引用需为合法环境变量名: {name}"));
    }
    if value.is_empty() {
        return Err("凭据值不能为空".to_string());
    }
    if value.chars().any(|c| c == '\n' || c == '\r') {
        return Err("凭据值不能包含换行".to_string());
    }
    let env = env_overrides_of(&state, &instance_id);
    if env.iter().any(|(k, _)| *k == name) {
        return Err(format!(
            "「{name}」已由实例环境变量提供（启动环境变量优先于凭据库），此处为只读"
        ));
    }
    let home = home_path_of(&state, &home_id)?;
    let path = home.join(CREDENTIALS_FILENAME);
    let raw = read_text(&path)?;
    ensure_unchanged(&raw, &expected_hash)?;
    let text = if raw.trim().is_empty() {
        // A fresh store keeps the DSH schema version marker.
        format!("version: 1\nrefs:\n  {name}: {}\n", yaml_scalar(&value)?)
    } else {
        splice_credential_ref(&raw, &name, &value)?
    };
    // Refuse to persist a document that no longer parses.
    parse_credential_refs(&text)?;
    write_text(&path, &text)?;
    crate::log_info!("已写入凭据引用 {name}: {}", path.display());
    list_credential_refs(state, home_id, instance_id)
}

/// Deletes one credential ref; the same shadowing rule as writes applies.
#[tauri::command]
pub fn delete_credential_ref(
    state: State<'_, AppState>,
    home_id: String,
    instance_id: String,
    name: String,
    expected_hash: String,
) -> Result<CredentialRefList, String> {
    let env = env_overrides_of(&state, &instance_id);
    if env.iter().any(|(k, _)| *k == name) {
        return Err(format!(
            "「{name}」已由实例环境变量提供（启动环境变量优先于凭据库），此处为只读"
        ));
    }
    let home = home_path_of(&state, &home_id)?;
    let path = home.join(CREDENTIALS_FILENAME);
    let raw = read_text(&path)?;
    ensure_unchanged(&raw, &expected_hash)?;
    let text = splice_credential_ref_removal(&raw, &name)?;
    // Refuse to persist a document that no longer parses.
    parse_credential_refs(&text)?;
    write_text(&path, &text)?;
    crate::log_info!("已删除凭据引用 {name}: {}", path.display());
    list_credential_refs(state, home_id, instance_id)
}

// ---------------------------------------------------------------------------
// Commands: pre-launch readiness check
// ---------------------------------------------------------------------------

fn check_item(code: &str, status: &str, params: &[(&str, String)]) -> ProviderCheckItem {
    ProviderCheckItem {
        code: code.to_string(),
        status: status.to_string(),
        params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
    }
}

/// `NAME=value` / `export NAME=value` lines of a dotenv file.
fn dotenv_names(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (key, _) = line.split_once('=')?;
            let key = key.trim();
            (!key.is_empty() && !key.starts_with('#')).then(|| key.to_string())
        })
        .collect()
}

/// Rates one status against another: warn > unknown > ok.
fn worst(a: &str, b: &str) -> String {
    let rank = |s: &str| match s {
        "warn" => 2,
        "unknown" => 1,
        _ => 0,
    };
    if rank(a) >= rank(b) {
        a.to_string()
    } else {
        b.to_string()
    }
}

/// Pre-launch readiness of every provider route: credential resolvability
/// (launch env > credential store > home `.env`), endpoint/protocol presence
/// for custom routes, model catalog inheritance for built-in routes. The
/// report is advisory and never blocks a launch.
#[tauri::command]
pub fn check_provider_routes(
    state: State<'_, AppState>,
    home_id: String,
    instance_id: String,
    profile: String,
) -> Result<Vec<ProviderRouteReport>, String> {
    let home = home_path_of(&state, &home_id)?;
    let path = profile_patch_path(&home, &profile)?;
    let raw = read_text(&path)?;
    let routes = parse_provider_routes(&raw)?;

    let env = env_overrides_of(&state, &instance_id);
    let creds_path = home.join(CREDENTIALS_FILENAME);
    let cred_refs: Vec<String> = parse_credential_refs(&read_text(&creds_path)?)?
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    let dotenv = dotenv_names(&home.join(".env"));

    let mut reports = Vec::new();
    for route in routes {
        let mut checks = Vec::new();
        // 1. Credential resolvability.
        if route.api_key_env.is_empty() {
            checks.push(check_item("noApiKeyEnv", "warn", &[]));
        } else if env.iter().any(|(k, _)| *k == route.api_key_env) {
            checks.push(check_item(
                "credentialFromEnv",
                "ok",
                &[("name", route.api_key_env.clone())],
            ));
        } else if cred_refs.contains(&route.api_key_env) {
            checks.push(check_item(
                "credentialFromStore",
                "ok",
                &[("name", route.api_key_env.clone())],
            ));
        } else if dotenv.contains(&route.api_key_env) {
            checks.push(check_item(
                "credentialFromDotenv",
                "ok",
                &[("name", route.api_key_env.clone())],
            ));
        } else {
            checks.push(check_item(
                "credentialMissing",
                "warn",
                &[("name", route.api_key_env.clone())],
            ));
        }
        // 2. Endpoint / protocol / models.
        if route.catalog {
            checks.push(check_item("catalogInherit", "ok", &[]));
        } else {
            if route.base_url.is_empty() {
                checks.push(check_item("missingBaseUrl", "warn", &[]));
            } else if !is_http_url_valid(&route.base_url) {
                checks.push(check_item("invalidBaseUrl", "warn", &[]));
            } else if route.base_url.starts_with("http://") && !is_loopback_http(&route.base_url) {
                checks.push(check_item("insecureBaseUrl", "warn", &[]));
            } else {
                checks.push(check_item("baseUrlOk", "ok", &[]));
            }
            if route.api.is_empty() {
                checks.push(check_item("missingApi", "warn", &[]));
            } else {
                checks.push(check_item(
                    "apiDeclared",
                    "ok",
                    &[("api", route.api.clone())],
                ));
            }
            if route.models.is_empty() {
                checks.push(check_item("modelsMissing", "unknown", &[]));
            } else {
                checks.push(check_item(
                    "modelsDeclared",
                    "ok",
                    &[("count", route.models.len().to_string())],
                ));
            }
        }
        let status = checks
            .iter()
            .fold("ok".to_string(), |acc, c| worst(&acc, &c.status));
        reports.push(ProviderRouteReport {
            route: route.route,
            status,
            checks,
        });
    }
    Ok(reports)
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile patch layer shaped like the one the DSH settings UI writes.
    const SAMPLE: &str = r#"# Your patch layer for this dsh profile, applied after every bundle layer:
# a top-level YAML array of loader patch entries (id-targeted config
# overrides, disables, and insert lists; `!!js` expressions allowed).
- id: ui-settings-general
  name: "@deepseek-ai/dsh-client-ui-settings-general"
  config:
    welcomeNoticeVersion: 2026-08-13.1
- id: llm-pi-ai
  name: "@deepseek-ai/dsh-llm-pi-ai"
  config:
    providers:
      anvilcraft-ai:
        displayName: " AnvilCraft AI"
        apiKeyEnv: ANVILCRAFT_AI_API_KEY
        api: openai-responses
        baseURL: https://ai.anvilcraft.dev
        models:
          - id: deepseek-v4.1-flash
            name: deepseek-v4.1-flash
            contextWindow: 1000000
            maxTokens: 384000
            input:
              - text
              - image
      mclans-ai:
        displayName: Mclans AI
        apiKeyEnv: MCLANS_AI_API_KEY
        api: openai-responses
        baseURL: https://sub2api.mclans.ink/
        models:
          - id: k3-256k
            name: k3-256k
            maxTokens: 384000
- id: agent-default-model
  name: "@deepseek-ai/dsh-agent-default-model"
  config:
    provider: anvilcraft-ai
    model: k3-256k
"#;

    const CREDS: &str = r#"version: 1
records:
  client-connection/browser-session:
    kind: grant
    payload:
      version: 1
      secret: XGDkmMjfA_pUILyqxhmvD_stBhYbi4G1nq20PSlsTzg
refs:
  ANVILCRAFT_AI_API_KEY: sk-ec93cc3d238a2fa97951e6d5a480c8b4d3b0e6c4df2780d5ec132972bb3fd842
  MCLANS_AI_API_KEY: sk-61101b3e564363b20f8d1d177e713bdbfb70306c94c5ee4a7ba3fe6f899ad4a0
"#;

    fn custom_route(route: &str) -> ProviderRoute {
        ProviderRoute {
            route: route.to_string(),
            display_name: "Test GW".to_string(),
            api_key_env: "TEST_API_KEY".to_string(),
            api: "openai-responses".to_string(),
            base_url: "https://gw.example.com".to_string(),
            models: vec![ProviderModel {
                id: "model-1".to_string(),
                name: String::new(),
                context_window: Some(131072),
                max_tokens: None,
                input: vec!["text".to_string()],
            }],
            extra: serde_json::Map::new(),
            catalog: false,
        }
    }

    #[test]
    fn parse_reads_routes_with_models_and_extra() {
        let routes = parse_provider_routes(SAMPLE).unwrap();
        assert_eq!(routes.len(), 2);
        let first = &routes[0];
        assert_eq!(first.route, "anvilcraft-ai");
        assert_eq!(first.api_key_env, "ANVILCRAFT_AI_API_KEY");
        assert_eq!(first.api, "openai-responses");
        assert_eq!(first.base_url, "https://ai.anvilcraft.dev");
        assert_eq!(first.models.len(), 1);
        assert_eq!(first.models[0].context_window, Some(1000000));
        assert_eq!(first.models[0].input, vec!["text", "image"]);
        // Second route kept its models without contextWindow.
        assert_eq!(routes[1].models[0].max_tokens, Some(384000));
        assert_eq!(routes[1].models[0].context_window, None);
    }

    #[test]
    fn splice_insert_appends_to_existing_providers() {
        let next = custom_route("my-gateway");
        let text = splice_route(SAMPLE, &next, None).unwrap();
        // The two hand-written routes survive byte-for-byte.
        assert!(text.contains("anvilcraft-ai:"));
        assert!(text.contains("displayName: \" AnvilCraft AI\""));
        assert!(text.contains("mclans-ai:"));
        // The new route landed inside the same entry's providers dict.
        assert!(text.contains("my-gateway:"));
        assert!(text.contains("TEST_API_KEY"));
        // Other entries are untouched.
        assert!(text.contains("- id: agent-default-model"));
        // Round-trip: three routes parse back out.
        let routes = parse_provider_routes(&text).unwrap();
        assert_eq!(routes.len(), 3);
        assert_eq!(routes[2].route, "my-gateway");
        assert_eq!(routes[2].models[0].id, "model-1");
    }

    #[test]
    fn splice_replace_keeps_siblings_byte_for_byte() {
        let mut edited = parse_provider_routes(SAMPLE).unwrap()[1].clone();
        edited.display_name = "Renamed GW".to_string();
        let text = splice_route(SAMPLE, &edited, Some("mclans-ai")).unwrap();
        let routes = parse_provider_routes(&text).unwrap();
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[1].display_name, "Renamed GW");
        // The first route's text is untouched, quotes and all.
        assert!(text.contains("displayName: \" AnvilCraft AI\""));
        assert!(text.contains("contextWindow: 1000000"));
    }

    #[test]
    fn splice_creates_entry_in_empty_document() {
        let raw = "# comment\n[]\n";
        let next = custom_route("solo");
        let text = splice_route(raw, &next, None).unwrap();
        assert!(text.contains("# comment"));
        assert!(!text.contains("[]"));
        assert!(text.contains("- id: llm-pi-ai"));
        assert!(text.contains("providers:"));
        let routes = parse_provider_routes(&text).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].route, "solo");
    }

    #[test]
    fn splice_adds_providers_to_entry_without_config() {
        let raw = "- id: llm-pi-ai\n  name: '@deepseek-ai/dsh-llm-pi-ai'\n";
        let next = custom_route("added");
        let text = splice_route(raw, &next, None).unwrap();
        let routes = parse_provider_routes(&text).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].route, "added");
    }

    #[test]
    fn splice_removal_drops_only_the_named_route() {
        let text = splice_route_removal(SAMPLE, "mclans-ai").unwrap();
        let routes = parse_provider_routes(&text).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].route, "anvilcraft-ai");
        assert!(!text.contains("MCLANS_AI_API_KEY"));
        assert!(text.contains("- id: agent-default-model"));
    }

    #[test]
    fn splice_removal_of_last_route_collapses_to_empty_dict() {
        let text = splice_route_removal(SAMPLE, "mclans-ai").unwrap();
        let text = splice_route_removal(&text, "anvilcraft-ai").unwrap();
        assert!(text.contains("providers: {}"));
        assert!(parse_provider_routes(&text).unwrap().is_empty());
        // The entry itself and the other entries survive.
        assert!(text.contains("- id: llm-pi-ai"));
        assert!(text.contains("- id: agent-default-model"));
    }

    #[test]
    fn validate_accepts_catalog_route_with_only_a_key() {
        let route = ProviderRoute {
            route: "deepseek".to_string(),
            api_key_env: "DEEPSEEK_API_KEY".to_string(),
            ..Default::default()
        };
        validate_route(&route, &[], None).unwrap();
    }

    #[test]
    fn validate_refuses_custom_route_without_endpoint() {
        let mut route = custom_route("gw");
        route.base_url.clear();
        assert!(validate_route(&route, &[], None).is_err());
        let mut route = custom_route("gw");
        route.api.clear();
        assert!(validate_route(&route, &[], None).is_err());
    }

    #[test]
    fn validate_refuses_duplicates_and_bad_names() {
        let existing = custom_route("gw");
        let dup = custom_route("gw");
        assert!(validate_route(&dup, &[existing], None).is_err());
        let bad = custom_route("bad route!");
        assert!(validate_route(&bad, &[], None).is_err());
        let bad_env = ProviderRoute {
            route: "deepseek".to_string(),
            api_key_env: "1BAD".to_string(),
            ..Default::default()
        };
        assert!(validate_route(&bad_env, &[], None).is_err());
    }

    #[test]
    fn validate_enforces_the_new_route_id_rule() {
        // New ids: lowercase start, only [a-z0-9_], no trailing underscore.
        assert!(validate_route(&custom_route("my_route_2"), &[], None).is_ok());
        assert!(validate_route(&custom_route("a"), &[], None).is_ok());
        assert!(validate_route(&custom_route("my-route"), &[], None).is_err());
        assert!(validate_route(&custom_route("MyRoute"), &[], None).is_err());
        assert!(validate_route(&custom_route("_route"), &[], None).is_err());
        assert!(validate_route(&custom_route("route_"), &[], None).is_err());
        assert!(validate_route(&custom_route("2route"), &[], None).is_err());
    }

    #[test]
    fn validate_grandfathers_an_unchanged_legacy_key() {
        // A kebab-case route the DSH settings UI wrote stays editable as long
        // as the key does not change; renaming it must follow the new rule.
        let legacy = ProviderRoute {
            route: "anvilcraft-ai".to_string(),
            api_key_env: "ANVILCRAFT_AI_API_KEY".to_string(),
            api: "openai-responses".to_string(),
            base_url: "https://ai.anvilcraft.dev".to_string(),
            ..Default::default()
        };
        validate_route(&legacy, &[], Some("anvilcraft-ai")).unwrap();
        assert!(validate_route(&legacy, &[], Some("other-route")).is_err());
        assert!(validate_route(&legacy, &[], None).is_err());
    }

    #[test]
    fn extra_keys_round_trip() {
        let mut route = parse_provider_routes(SAMPLE).unwrap()[0].clone();
        route
            .extra
            .insert("timeoutMs".to_string(), serde_json::json!(60000));
        let text = splice_route(SAMPLE, &route, Some("anvilcraft-ai")).unwrap();
        let back = parse_provider_routes(&text).unwrap();
        assert_eq!(
            back[0].extra.get("timeoutMs"),
            Some(&serde_json::json!(60000))
        );
    }

    #[test]
    fn credentials_parse_and_mask() {
        let refs = parse_credential_refs(CREDS).unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].0, "ANVILCRAFT_AI_API_KEY");
        let masked = mask_secret(&refs[0].1);
        assert!(masked.starts_with("sk-e"));
        assert!(masked.ends_with("d842"));
        assert!(!masked.contains("cc3d238a"));
        // Short secrets get a fixed-width mask so their length stays secret.
        assert_eq!(mask_secret("short"), "********");
        assert_eq!(mask_secret("12345678"), "********");
    }

    #[test]
    fn credential_splice_preserves_records_and_replaces() {
        let text =
            splice_credential_ref(CREDS, "MCLANS_AI_API_KEY", "sk-new-value-123456").unwrap();
        let refs = parse_credential_refs(&text).unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[1].1, "sk-new-value-123456");
        // The records section survives byte-for-byte.
        assert!(text.contains("client-connection/browser-session"));
        assert!(text.contains("secret: XGDkmMjfA_pUILyqxhmvD_stBhYbi4G1nq20PSlsTzg"));
        // Insert a third ref.
        let text = splice_credential_ref(&text, "THIRD_KEY", "abc123").unwrap();
        let refs = parse_credential_refs(&text).unwrap();
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[2].0, "THIRD_KEY");
    }

    #[test]
    fn credential_removal_collapses_last_entry() {
        let text = splice_credential_ref_removal(CREDS, "ANVILCRAFT_AI_API_KEY").unwrap();
        let refs = parse_credential_refs(&text).unwrap();
        assert_eq!(refs.len(), 1);
        let text = splice_credential_ref_removal(&text, "MCLANS_AI_API_KEY").unwrap();
        assert!(text.contains("refs: {}"));
        assert!(text.contains("records:"));
    }

    #[test]
    fn dotenv_parser_finds_names() {
        let dir = std::env::temp_dir().join(format!("dsh-dotenv-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".env"),
            "# comment\nA=1\nexport B='x'\nC = spaced\n",
        )
        .unwrap();
        let names = dotenv_names(&dir.join(".env"));
        assert!(names.contains(&"A".to_string()));
        assert!(names.contains(&"B".to_string()));
        assert!(names.contains(&"C".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rename_of_last_route_reexpands_collapsed_providers() {
        // Deleting the last route collapses the mapping to `providers: {}`;
        // re-inserting (the second half of a rename) must expand the key
        // back to block form instead of appending below the inline value.
        let one = splice_route_removal(SAMPLE, "mclans-ai").unwrap();
        let collapsed = splice_route_removal(&one, "anvilcraft-ai").unwrap();
        assert!(collapsed.contains("providers: {}"));
        let renamed = splice_route(&collapsed, &custom_route("renamed"), None).unwrap();
        assert!(!renamed.contains("providers: {}"));
        let routes = parse_provider_routes(&renamed).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].route, "renamed");
    }

    #[test]
    fn credential_set_after_last_removal_reexpands_refs() {
        // `refs: {}` is the collapsed end state of deleting the last ref;
        // setting a new one must not append below the inline value.
        let text = splice_credential_ref_removal(CREDS, "ANVILCRAFT_AI_API_KEY").unwrap();
        let text = splice_credential_ref_removal(&text, "MCLANS_AI_API_KEY").unwrap();
        assert!(text.contains("refs: {}"));
        let text = splice_credential_ref(&text, "NEW_KEY", "sk-123456789").unwrap();
        assert!(!text.contains("refs: {}"));
        let refs = parse_credential_refs(&text).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].0, "NEW_KEY");
        assert!(text.contains("records:"));
    }

    #[test]
    fn ensure_unchanged_refuses_stale_hash() {
        let raw = "content";
        let hash = sha256_hex(raw);
        ensure_unchanged(raw, &hash).unwrap();
        ensure_unchanged(raw, "").unwrap(); // empty = guard skipped
        let err = ensure_unchanged("tampered", &hash).unwrap_err();
        assert!(err.starts_with("STALE_HASH:"));
    }

    #[test]
    fn comments_inside_providers_mapping_do_not_break_splices() {
        // Hand-written comments are free-floating in YAML: they neither end
        // the mapping nor count as route keys. A comment at the providers-key
        // indent used to truncate the span scan, silently dropping every
        // route below it from replace/delete decisions.
        let raw = r#"- id: llm-pi-ai
  name: '@deepseek-ai/dsh-llm-pi-ai'
  config:
    providers:
      deepseek:
        apiKeyEnv: DEEPSEEK_API_KEY
    # section break
      openai:
        apiKeyEnv: OPENAI_API_KEY
"#;
        // Deleting the first route must keep the comment and the route
        // below it (previously the last-route collapse destroyed both).
        let text = splice_route_removal(raw, "deepseek").unwrap();
        assert!(text.contains("# section break"));
        let routes = parse_provider_routes(&text).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].route, "openai");
        // Replacing the route below the comment must not duplicate it.
        let mut edited = parse_provider_routes(raw).unwrap()[1].clone();
        edited.display_name = "Edited".to_string();
        let text = splice_route(raw, &edited, Some("openai")).unwrap();
        let routes = parse_provider_routes(&text).unwrap();
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[1].display_name, "Edited");
        assert!(text.contains("# section break"));
    }

    #[test]
    fn comments_inside_refs_map_do_not_duplicate_entries() {
        // A comment at column 0 inside the refs map used to end the span
        // scan: editing a ref below the comment then took the append branch
        // and wrote a duplicate key.
        let raw = "version: 1\nrefs:\n  A_KEY: aaa111\n# note\n  B_KEY: bbb222\n";
        let text = splice_credential_ref(raw, "B_KEY", "ccc333").unwrap();
        let refs = parse_credential_refs(&text).unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[1], ("B_KEY".to_string(), "ccc333".to_string()));
        assert!(text.contains("# note"));
    }

    #[test]
    fn hand_written_deep_indent_keeps_new_block_aligned() {
        // Routes indented deeper than serde_yaml's +2 must get new siblings
        // at their own indent, or the file ends up with mixed indents.
        let raw = r#"- id: llm-pi-ai
  name: '@deepseek-ai/dsh-llm-pi-ai'
  config:
    providers:
        deepseek:
            apiKeyEnv: DEEPSEEK_API_KEY
"#;
        let text = splice_route(raw, &custom_route("added"), None).unwrap();
        assert!(text.contains("        added:"));
        let routes = parse_provider_routes(&text).unwrap();
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[1].route, "added");
        // Replacing the deep-indented route keeps the indent too.
        let mut edited = parse_provider_routes(raw).unwrap()[0].clone();
        edited.display_name = "Edited".to_string();
        let text = splice_route(raw, &edited, Some("deepseek")).unwrap();
        let routes = parse_provider_routes(&text).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].display_name, "Edited");
    }

    #[test]
    fn inline_empty_config_expands_before_inserting_providers() {
        // `config: {}` cannot take block children: the key line collapses to
        // its bare form first.
        let raw = "- id: llm-pi-ai\n  name: '@deepseek-ai/dsh-llm-pi-ai'\n  config: {}\n";
        let text = splice_route(raw, &custom_route("added"), None).unwrap();
        assert!(!text.contains("config: {}"));
        let routes = parse_provider_routes(&text).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].route, "added");
    }

    #[test]
    fn non_empty_inline_flow_is_refused_not_dropped() {
        // `refs: {A: x}` rewritten to a bare key would silently drop A.
        let raw = "version: 1\nrefs: {A_KEY: aaa111}\n";
        let err = splice_credential_ref(raw, "B_KEY", "bbb222").unwrap_err();
        assert!(err.contains("内联 flow"));
        // Same guard for the providers key.
        let raw = "- id: llm-pi-ai\n  name: '@deepseek-ai/dsh-llm-pi-ai'\n  config:\n    providers: {deepseek: {apiKeyEnv: K}}\n";
        let err = splice_route(raw, &custom_route("added"), None).unwrap_err();
        assert!(err.contains("内联 flow"));
        // And for a non-empty inline config.
        let raw = "- id: llm-pi-ai\n  name: '@deepseek-ai/dsh-llm-pi-ai'\n  config: {other: 1}\n";
        let err = splice_route(raw, &custom_route("added"), None).unwrap_err();
        assert!(err.contains("内联 flow"));
    }

    #[test]
    fn dash_line_inline_config_expands_and_keeps_list_marker() {
        // The config key may sit on the entry's own `- ` line; collapsing
        // `- config: {}` must keep the list marker or the entry breaks.
        let raw = "- config: {}\n  id: llm-pi-ai\n  name: '@deepseek-ai/dsh-llm-pi-ai'\n";
        let text = splice_route(raw, &custom_route("added"), None).unwrap();
        assert!(text.contains("- config:"));
        let routes = parse_provider_routes(&text).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].route, "added");
        // A non-empty inline flow on the dash line is still refused.
        let raw = "- config: {other: 1}\n  id: llm-pi-ai\n  name: '@deepseek-ai/dsh-llm-pi-ai'\n";
        let err = splice_route(raw, &custom_route("added"), None).unwrap_err();
        assert!(err.contains("内联 flow"));
    }

    #[test]
    fn nested_providers_key_is_not_hijacked() {
        // A `providers:` key deeper than a direct config child belongs to
        // some other sub-mapping; the route table must be created fresh.
        let raw = r#"- id: llm-pi-ai
  name: '@deepseek-ai/dsh-llm-pi-ai'
  config:
    experimental:
      providers:
        fake: {}
"#;
        let text = splice_route(raw, &custom_route("added"), None).unwrap();
        assert!(text.contains("experimental:"));
        assert!(text.contains("fake: {}"));
        let routes = parse_provider_routes(&text).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].route, "added");
    }

    #[test]
    fn missing_trailing_newline_stays_missing() {
        let raw = "version: 1\nrefs:\n  A_KEY: aaa111";
        let text = splice_credential_ref(raw, "B_KEY", "bbb222").unwrap();
        assert!(!text.ends_with('\n'));
        let text = splice_credential_ref_removal(raw, "A_KEY").unwrap();
        assert!(!text.ends_with('\n'));
    }

    #[test]
    fn loopback_http_detection_requires_a_real_loopback_host() {
        assert!(is_loopback_http("http://localhost:3000/v1"));
        assert!(is_loopback_http("http://127.0.0.1:8080"));
        assert!(!is_loopback_http("http://127.evil.com"));
        assert!(!is_loopback_http("http://192.168.1.10"));
        assert!(!is_loopback_http("https://localhost"));
    }
}
