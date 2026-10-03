//! Model provider configuration in the DSH patch layer (issue #89).
//!
//! DSH configures model providers from its own **Settings → Models** page, and
//! this module lets the launcher do the same thing with the same semantics:
//!
//! * routes live in `llm-pi-ai.config.providers` of the profile's
//!   `cordis.patch.yml` — the exact node DSH's own settings editor writes;
//! * the built-in provider catalogue is read straight off the installed
//!   `@earendil-works/pi-ai` package, so the launcher offers the same
//!   providers, endpoints, protocols and model lists without running DSH;
//! * secrets are write-only and live in `$DSH_HOME/.credentials.yaml`
//!   (see [`crate::credentials`]); the patch only ever keeps the reference.
//!
//! Two invariants drive the design:
//!
//! 1. **The form is deliberately small.** DSH exposes only what a route needs
//!    to exist (key, display name, base URL, protocol, and per model: id,
//!    name, context window, max tokens). Everything else — `compat`,
//!    `modelOverrides`, `reasoningEfforts`, `retryPolicy`, `headers`,
//!    timeouts — stays in the file and is **never** surfaced.
//! 2. **Saving must not drop what the form does not show.** A save therefore
//!    starts from the route's parsed YAML mapping and overwrites only the
//!    managed keys, then splices those lines back into the raw text, so
//!    comments, key order and unmanaged keys survive byte-for-byte.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::credentials::CredentialInfo;
use crate::AppState;

/// The loader module owning the `providers` dictionary.
const PIAI_MODULE: &str = "@deepseek-ai/dsh-llm-pi-ai";
/// Patch-layer filename inside a DSH_HOME / profile directory.
const PATCH_FILENAME: &str = "cordis.patch.yml";
/// Synthetic route id of the DeepSeek card DSH always shows first.
const DEEPSEEK_ID: &str = "deepseek-official";
/// The DeepSeek card's credential reference is fixed, never derived.
const DEEPSEEK_REF: &str = "DEEPSEEK_API_KEY";
/// Default endpoint of the DeepSeek card, shown read-only.
const DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com/anthropic";
/// Wire protocols pi-ai can read a model list for.
const LISTABLE_PROTOCOLS: [&str; 3] = [
    "anthropic-messages",
    "openai-completions",
    "openai-responses",
];
/// Response body cap for model discovery, mirrored from pi-ai.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const ANTHROPIC_VERSION: &str = "2023-06-01";
const ANTHROPIC_MODEL_LIMIT: u32 = 1000;
/// How long a parsed catalogue is reused before the package is read again.
const CATALOG_TTL: std::time::Duration = std::time::Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// One model entry as the form shows it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub context_window: Option<u64>,
    pub max_tokens: Option<u64>,
}

/// One configured route: a `providers` key plus what the form edits.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRoute {
    /// The `providers` dictionary key — permanent, never renamed.
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    /// Credential reference; derived from the id when left empty.
    #[serde(default)]
    pub api_key_env: String,
    #[serde(default)]
    pub api: String,
    #[serde(default)]
    pub base_url: String,
    /// True when the installed catalogue ships this provider, so its
    /// endpoint, protocol and model list come from the catalogue.
    #[serde(default)]
    pub catalog: bool,
    /// True for the synthetic DeepSeek card, which is credential-only.
    #[serde(default)]
    pub official: bool,
    #[serde(default)]
    pub models: Vec<ProviderModel>,
    /// Route keys the form does not surface; shown as "kept as-is".
    #[serde(default)]
    pub extra_keys: Vec<String>,
    /// Filled by `list`; ignored by `save`.
    #[serde(default)]
    pub credential: Option<CredentialInfo>,
}

/// A provider the installed catalogue can supply.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CatalogProvider {
    pub id: String,
    pub name: String,
    pub api: String,
    pub base_url: String,
    pub model_count: usize,
    /// False when pi-ai offers no API-key auth for it (OAuth-only), in which
    /// case DSH does not list it either.
    pub api_key: bool,
}

/// A model as the catalogue or a discovery response describes it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub context_window: Option<u64>,
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub input: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCatalog {
    pub providers: Vec<CatalogProvider>,
    /// Set when part of the catalogue could not be read.
    pub notice: Option<String>,
}

/// Input for model discovery; mirrors what the form currently holds.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverInput {
    pub instance_id: String,
    pub home_id: String,
    pub profile: Option<String>,
    /// Catalogue provider id when the form is adding a built-in provider.
    pub provider: Option<String>,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api: String,
    #[serde(default)]
    pub api_key: String,
    /// Route being edited, so a stored key can be reused.
    pub route_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Grammar
// ---------------------------------------------------------------------------

/// `deriveKeyRef` in DSH: `moonshotai` → `MOONSHOTAI_API_KEY`.
pub fn derive_key_ref(id: &str) -> String {
    let upper = id.to_uppercase();
    let mut out = String::new();
    let mut pending = false;
    for ch in upper.chars() {
        if ch.is_ascii_uppercase() || ch.is_ascii_digit() {
            out.push(ch);
            pending = false;
        } else if !pending {
            out.push('_');
            pending = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    format!("{out}_API_KEY")
}

fn validate_route_id(id: &str) -> Result<(), String> {
    let bytes = id.as_bytes();
    let ok = !bytes.is_empty()
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
    if ok {
        Ok(())
    } else {
        Err("Provider ID 需以小写字母开头，之后可用小写字母、数字和短横线。".to_string())
    }
}

fn validate_base_url(url: &str) -> Result<(), String> {
    let ok = (url.starts_with("http://") || url.starts_with("https://")) && url.len() > 8;
    if ok {
        Ok(())
    } else {
        Err("请输入有效的 HTTP 或 HTTPS 地址。".to_string())
    }
}

fn validate_route(route: &ProviderRoute, taken: &[String], catalog: &[String]) -> Result<(), String> {
    if route.id.is_empty() {
        return Err("请填写 Provider ID。".to_string());
    }
    validate_route_id(&route.id)?;
    if taken.iter().any(|t| t == &route.id) {
        return Err("已有提供商使用了这个 ID。".to_string());
    }
    let builtin = catalog.iter().any(|c| c == &route.id);
    if builtin {
        return Ok(());
    }
    validate_base_url(&route.base_url)?;
    if !LISTABLE_PROTOCOLS.contains(&route.api.as_str()) {
        return Err("请选择 API 协议。".to_string());
    }
    if route.models.is_empty() {
        return Err("自定义模型 API 至少需要一个模型。".to_string());
    }
    let mut seen = std::collections::BTreeSet::new();
    for model in &route.models {
        if model.id.trim().is_empty() {
            return Err("模型 ID 不能为空。".to_string());
        }
        if !seen.insert(model.id.clone()) {
            return Err("模型 ID 不能重复。".to_string());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Catalogue
// ---------------------------------------------------------------------------

static CATALOG_CACHE: std::sync::Mutex<Vec<(PathBuf, std::time::Instant, ProviderCatalog)>> =
    std::sync::Mutex::new(Vec::new());

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 {
        let first = trimmed.as_bytes()[0];
        if (first == b'\'' || first == b'"') && trimmed.ends_with(first as char) {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

fn key_of(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("- ") {
        return None;
    }
    let (key, _) = trimmed.split_once(':')?;
    Some(key.trim().to_string())
}

fn inline_value(line: &str) -> String {
    match line.trim().split_once(':') {
        Some((_, rest)) => rest.trim().to_string(),
        None => String::new(),
    }
}

/// Locates the installed `@earendil-works/pi-ai` package. pnpm encodes the
/// version and peer-dependency hashes in the directory name, so the `.pnpm`
/// entry is matched by prefix and the highest version wins.
fn pi_ai_dir(version_dir: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    let pnpm = version_dir.join("node_modules").join(".pnpm");
    if let Ok(entries) = std::fs::read_dir(&pnpm) {
        let mut best: Option<(semver::Version, PathBuf)> = None;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(rest) = name.strip_prefix("@earendil-works+pi-ai@") else {
                continue;
            };
            let version = rest.split('_').next().unwrap_or_default();
            let Ok(parsed) = semver::Version::parse(version) else {
                continue;
            };
            let dir = entry
                .path()
                .join("node_modules")
                .join("@earendil-works")
                .join("pi-ai");
            if !dir.join("dist").join("providers").join("data").is_dir() {
                continue;
            }
            if best.as_ref().map(|(v, _)| &parsed > v).unwrap_or(true) {
                best = Some((parsed, dir));
            }
        }
        if let Some((_, dir)) = best {
            candidates.push(dir);
        }
    }

    candidates.push(
        version_dir
            .join("node_modules")
            .join("@earendil-works")
            .join("pi-ai"),
    );
    candidates.push(
        version_dir
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh-llm-pi-ai")
            .join("node_modules")
            .join("@earendil-works")
            .join("pi-ai"),
    );

    candidates
        .into_iter()
        .find(|dir| dir.join("dist").join("providers").join("data").is_dir())
}

/// Provider display name, read from the compiled provider module. Falls back
/// to the id: the id is what DSH's own picker shows anyway.
fn provider_name(dir: &Path, id: &str) -> String {
    let Ok(source) = std::fs::read_to_string(dir.join("dist").join("providers").join(format!("{id}.js"))) else {
        return id.to_string();
    };
    let pattern = format!(r#"id:\s*"{id}",\s*name:\s*"([^"]+)""#);
    let Ok(regex) = regex::Regex::new(&pattern) else {
        return id.to_string();
    };
    regex
        .captures(&source)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| id.to_string())
}

/// True when pi-ai offers API-key authentication for this provider; OAuth-only
/// providers (DSH does not list them) are the exception. An unreadable module
/// is treated as supporting keys: wrongly offering a provider only leads to a
/// failed probe, while wrongly hiding one removes it entirely.
fn provider_supports_api_key(dir: &Path, id: &str) -> bool {
    let Ok(source) = std::fs::read_to_string(dir.join("dist").join("providers").join(format!("{id}.js"))) else {
        return true;
    };
    match regex::Regex::new(r"\bapiKey\s*:") {
        Ok(regex) => regex.is_match(&source),
        Err(_) => true,
    }
}

fn parse_catalog_file(bytes: &[u8]) -> Option<(String, String, usize)> {
    let doc: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let groups = doc.as_object()?;
    let mut best: Option<(String, &serde_json::Map<String, serde_json::Value>)> = None;
    let mut total = 0usize;
    for (api, group) in groups {
        let Some(entries) = group.as_object() else {
            continue;
        };
        total += entries.len();
        if best
            .as_ref()
            .map(|(_, current)| entries.len() > current.len())
            .unwrap_or(true)
        {
            best = Some((api.clone(), entries));
        }
    }
    let (api, entries) = best?;
    if entries.is_empty() {
        return None;
    }
    let base_url = entries
        .values()
        .find_map(|v| v.get("baseUrl").and_then(|b| b.as_str()))
        .unwrap_or_default()
        .to_string();
    Some((api, base_url, total))
}

/// Reads the catalogue off disk. Returns the package directory as well so the
/// caller can cache and re-read models for one provider.
fn load_catalog_at(version_dir: &Path) -> Result<(PathBuf, ProviderCatalog), String> {
    let dir = pi_ai_dir(version_dir).ok_or_else(|| {
        format!(
            "未在安装目录中找到 @earendil-works/pi-ai（{}）；内置提供方列表不可用，仍可添加自定义提供方",
            version_dir.display()
        )
    })?;
    let data = dir.join("dist").join("providers").join("data");
    let entries = std::fs::read_dir(&data)
        .map_err(|e| format!("读取提供方目录失败: {e}"))?;

    let mut providers: Vec<CatalogProvider> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".json") || name.starts_with('.') {
            continue;
        }
        let id = name.trim_end_matches(".json").to_string();
        let Ok(bytes) = std::fs::read(entry.path()) else {
            failed.push(id);
            continue;
        };
        let Some((api, base_url, model_count)) = parse_catalog_file(&bytes) else {
            failed.push(id);
            continue;
        };
        providers.push(CatalogProvider {
            name: provider_name(&dir, &id),
            api_key: provider_supports_api_key(&dir, &id),
            id,
            api,
            base_url,
            model_count,
        });
    }
    if providers.is_empty() {
        return Err(format!(
            "安装目录 {} 中的提供方目录为空",
            version_dir.display()
        ));
    }
    providers.sort_by(|a, b| a.id.cmp(&b.id));
    let notice = if failed.is_empty() {
        None
    } else {
        Some(format!("有 {} 个内置提供方未能读取：{}", failed.len(), failed.join(", ")))
    };
    Ok((dir, ProviderCatalog { providers, notice }))
}

fn cached_catalog(version_dir: &Path) -> Result<(PathBuf, ProviderCatalog), String> {
    {
        let mut cache = CATALOG_CACHE.lock().unwrap();
        cache.retain(|(_, at, _)| at.elapsed() < CATALOG_TTL);
        if let Some((dir, _, catalog)) = cache.iter().find(|(d, _, _)| d == version_dir) {
            return Ok((dir.clone(), catalog.clone()));
        }
    }
    let (dir, catalog) = load_catalog_at(version_dir)?;
    let mut cache = CATALOG_CACHE.lock().unwrap();
    cache.retain(|(d, _, _)| d != version_dir);
    cache.push((version_dir.to_path_buf(), std::time::Instant::now(), catalog.clone()));
    Ok((dir, catalog))
}

/// Model list of one catalogue provider, read straight from its JSON file.
fn catalog_models_at(dir: &Path, provider_id: &str) -> Result<Vec<CatalogModel>, String> {
    let path = dir
        .join("dist")
        .join("providers")
        .join("data")
        .join(format!("{provider_id}.json"));
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("读取提供方模型失败: {e}"))?;
    let doc: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("解析提供方模型失败: {e}"))?;
    let mut out: Vec<CatalogModel> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    if let Some(groups) = doc.as_object() {
        for entries in groups.values() {
            let Some(entries) = entries.as_object() else {
                continue;
            };
            for (key, entry) in entries {
                let id = entry
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| key.clone());
                if id.is_empty() || !seen.insert(id.clone()) {
                    continue;
                }
                let name = entry
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| id.clone());
                out.push(CatalogModel {
                    id,
                    name,
                    context_window: entry
                        .get("contextWindow")
                        .and_then(|v| v.as_u64())
                        .filter(|v| *v > 0),
                    max_tokens: entry
                        .get("maxTokens")
                        .and_then(|v| v.as_u64())
                        .filter(|v| *v > 0),
                    input: entry
                        .get("input")
                        .and_then(|v| v.as_array())
                        .map(|list| {
                            list.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default(),
                });
            }
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

// ---------------------------------------------------------------------------
// Patch reading
// ---------------------------------------------------------------------------

/// One `providers` entry: the dictionary key and its parsed mapping.
#[derive(Clone, Debug)]
pub struct RouteEntry {
    pub id: String,
    pub map: serde_yaml::Mapping,
}

fn yaml_str(value: &serde_yaml::Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

/// Reads the `providers` dictionary of the `llm-pi-ai` entry, in file order.
pub fn read_routes(raw: &str) -> Result<Vec<RouteEntry>, String> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let doc: serde_yaml::Value =
        serde_yaml::from_str(raw).map_err(|e| format!("{PATCH_FILENAME} 不是合法的 YAML: {e}"))?;
    let Some(items) = doc.as_sequence() else {
        return Err(format!("{PATCH_FILENAME} 的顶层不是数组"));
    };
    for item in items {
        let Some(entry) = item.as_mapping() else {
            continue;
        };
        if entry
            .get("name")
            .map(yaml_str)
            .as_deref()
            .map(unquote)
            .as_deref()
            != Some(PIAI_MODULE)
        {
            continue;
        }
        let Some(providers) = entry
            .get("config")
            .and_then(|c| c.get("providers"))
            .and_then(|p| p.as_mapping())
        else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for (key, value) in providers {
            let id = yaml_str(key);
            if id.is_empty() {
                continue;
            }
            if let Some(map) = value.as_mapping() {
                out.push(RouteEntry { id, map: map.clone() });
            }
        }
        return Ok(out);
    }
    Ok(Vec::new())
}

/// Projects one parsed route onto the wire type, keeping the unmanaged keys
/// listed so the form can say they are preserved.
fn to_wire(entry: &RouteEntry, catalog: &[String], credential: Option<CredentialInfo>) -> ProviderRoute {
    let get = |key: &str| entry.map.get(key).cloned();
    let managed = ["apiKeyEnv", "displayName", "api", "baseURL", "models"];
    let extra_keys = entry
        .map
        .keys()
        .filter_map(|k| k.as_str().map(|s| s.to_string()))
        .filter(|k| !managed.contains(&k.as_str()))
        .collect::<Vec<_>>();

    let models = get("models")
        .and_then(|v| v.as_sequence().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|item| item.as_mapping())
        .map(|map| ProviderModel {
            id: map.get("id").map(yaml_str).unwrap_or_default(),
            name: map.get("name").map(yaml_str).unwrap_or_default(),
            context_window: map.get("contextWindow").and_then(|v| v.as_u64()),
            max_tokens: map.get("maxTokens").and_then(|v| v.as_u64()),
        })
        .filter(|m| !m.id.is_empty())
        .collect::<Vec<_>>();

    ProviderRoute {
        id: entry.id.clone(),
        display_name: get("displayName").map(|v| yaml_str(&v)).unwrap_or_default(),
        api_key_env: get("apiKeyEnv").map(|v| yaml_str(&v)).unwrap_or_default(),
        api: get("api").map(|v| yaml_str(&v)).unwrap_or_default(),
        base_url: get("baseURL").map(|v| yaml_str(&v)).unwrap_or_default(),
        catalog: catalog.iter().any(|c| c == &entry.id),
        official: false,
        models,
        extra_keys,
        credential,
    }
}

fn set_str(map: &mut serde_yaml::Mapping, key: &str, value: &str) {
    map.insert(
        serde_yaml::Value::String(key.to_string()),
        serde_yaml::Value::String(value.to_string()),
    );
}

fn set_u64(map: &mut serde_yaml::Mapping, key: &str, value: u64) {
    map.insert(
        serde_yaml::Value::String(key.to_string()),
        serde_yaml::Value::Number(value.into()),
    );
}

/// Applies the form onto a route, starting from the parsed mapping so every key
/// the form does not own is carried over untouched.
///
/// `api` / `baseURL` are only written when the form supplied them: a built-in
/// provider's endpoint and protocol come from the catalogue, and the form never
/// shows them, so an empty value must not invent one.
pub fn apply_route(route: &ProviderRoute, existing: Option<&serde_yaml::Mapping>) -> serde_yaml::Mapping {
    let mut map = existing.cloned().unwrap_or_default();

    let reference = if route.api_key_env.trim().is_empty() {
        derive_key_ref(&route.id)
    } else {
        route.api_key_env.trim().to_string()
    };
    set_str(&mut map, "apiKeyEnv", &reference);

    if route.display_name.trim().is_empty() {
        map.remove("displayName");
    } else {
        set_str(&mut map, "displayName", route.display_name.trim());
    }

    if !route.api.trim().is_empty() {
        set_str(&mut map, "api", route.api.trim());
    }
    if !route.base_url.trim().is_empty() {
        set_str(&mut map, "baseURL", route.base_url.trim());
    }

    if route.models.is_empty() {
        map.remove("models");
    } else {
        let existing_models = map
            .get("models")
            .and_then(|v| v.as_sequence().cloned())
            .unwrap_or_default();
        let by_id = |id: &str| {
            existing_models
                .iter()
                .filter_map(|item| item.as_mapping())
                .find(|m| m.get("id").map(yaml_str).as_deref() == Some(id))
                .cloned()
        };
        let mut models = Vec::new();
        for model in &route.models {
            let mut entry = by_id(&model.id).unwrap_or_default();
            entry.insert(
                serde_yaml::Value::String("id".to_string()),
                serde_yaml::Value::String(model.id.clone()),
            );
            if model.name.trim().is_empty() {
                entry.remove("name");
            } else {
                set_str(&mut entry, "name", model.name.trim());
            }
            match model.context_window {
                Some(value) if value > 0 => set_u64(&mut entry, "contextWindow", value),
                _ => {
                    entry.remove("contextWindow");
                }
            }
            match model.max_tokens {
                Some(value) if value > 0 => set_u64(&mut entry, "maxTokens", value),
                _ => {
                    entry.remove("maxTokens");
                }
            }
            models.push(serde_yaml::Value::Mapping(entry));
        }
        map.insert(
            serde_yaml::Value::String("models".to_string()),
            serde_yaml::Value::Sequence(models),
        );
    }
    map
}

// ---------------------------------------------------------------------------
// Patch writing (line-level splice)
// ---------------------------------------------------------------------------

/// Ranges of the top-level sequence items (`- id: …` at indent 0).
fn top_entries(lines: &[String]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if indent_of(line) == 0 && line.trim_start().starts_with("- ") {
            if let Some(open) = start.take() {
                out.push((open, i));
            }
            start = Some(i);
        }
    }
    if let Some(open) = start {
        out.push((open, lines.len()));
    }
    out
}

/// Last non-blank line inside a range, plus one — where appended text belongs.
fn insert_at(lines: &[String], start: usize, end: usize) -> usize {
    let mut at = end;
    while at > start && lines[at - 1].trim().is_empty() {
        at -= 1;
    }
    at
}

/// Finds a key at one indent inside a range; returns its line and the end of
/// its block (exclusive, trailing blanks trimmed).
fn find_key(lines: &[String], start: usize, end: usize, indent: usize, key: &str) -> Option<(usize, usize)> {
    for i in start..end {
        if indent_of(&lines[i]) != indent || key_of(&lines[i]).as_deref() != Some(key) {
            continue;
        }
        let mut block_end = end;
        for j in (i + 1)..end {
            let candidate = &lines[j];
            if candidate.trim().is_empty() || indent_of(candidate) > indent {
                continue;
            }
            block_end = j;
            break;
        }
        let mut trimmed = block_end;
        while trimmed > i + 1 && lines[trimmed - 1].trim().is_empty() {
            trimmed -= 1;
        }
        return Some((i, trimmed));
    }
    None
}

fn entry_module(lines: &[String], start: usize, end: usize) -> Option<String> {
    if let Some((line, _)) = find_key(lines, start, end, 2, "name") {
        return Some(unquote(&inline_value(&lines[line])));
    }
    let head = lines[start].trim_start().trim_start_matches("- ");
    if key_of(head).as_deref() == Some("name") {
        return Some(unquote(&inline_value(head)));
    }
    None
}

enum Spot {
    /// `providers:` already exists; replace its content.
    Existing { key_line: usize, content_end: usize, indent: usize },
    /// `config:` exists but has no `providers:`; insert inside it.
    UnderConfig { at: usize, indent: usize },
    /// The entry exists but has no `config:`; insert inside the entry.
    UnderEntry { at: usize },
    /// No `llm-pi-ai` entry at all; append one.
    AppendEntry,
}

fn locate_providers(lines: &[String]) -> Result<Spot, String> {
    for (start, end) in top_entries(lines) {
        if entry_module(lines, start, end).as_deref() != Some(PIAI_MODULE) {
            continue;
        }
        let Some((config_line, config_end)) = find_key(lines, start, end, 2, "config") else {
            return Ok(Spot::UnderEntry {
                at: insert_at(lines, start, end),
            });
        };
        if !inline_value(&lines[config_line]).is_empty() {
            return Err(
                "llm-pi-ai 条目的 config 是内联写法，启动器无法安全改写，请改为分行写法".to_string(),
            );
        }
        match find_key(lines, config_line + 1, config_end, 4, "providers") {
            Some((key_line, content_end)) => {
                return Ok(Spot::Existing {
                    key_line,
                    content_end,
                    indent: indent_of(&lines[key_line]),
                })
            }
            None => {
                return Ok(Spot::UnderConfig {
                    at: insert_at(lines, config_line + 1, config_end),
                    indent: 4,
                })
            }
        }
    }
    Ok(Spot::AppendEntry)
}

/// Renders one route mapping under its dictionary key, emitting nested YAML
/// exactly the way DSH's own editor does (sequences indented one level deeper
/// than their parent key). A recursive emitter avoids the off-by-two mistakes
/// a line-based re-indenter makes on nested sequences.
fn render_route(indent: usize, id: &str, map: &serde_yaml::Mapping) -> Result<Vec<String>, String> {
    let mut out = vec![format!("{}{}:", " ".repeat(indent), id)];
    emit_mapping_body(map, indent + 2, &mut out);
    Ok(out)
}

/// Inline scalar text for one value, preserving serde_yaml quoting.
fn yaml_scalar(v: &serde_yaml::Value) -> String {
    serde_yaml::to_string(v)
        .map(|s| s.trim_end().to_string())
        .unwrap_or_default()
}

/// Emits `key: value` handling mappings and sequences recursively.
fn emit_key_value(key: &str, v: &serde_yaml::Value, indent: usize, out: &mut Vec<String>) {
    let pad = " ".repeat(indent);
    match v {
        serde_yaml::Value::Mapping(_) => {
            out.push(format!("{}{}:", pad, key));
            emit_value_body(v, indent + 2, out);
        }
        serde_yaml::Value::Sequence(_) => {
            out.push(format!("{}{}:", pad, key));
            for item in v.as_sequence().unwrap() {
                emit_seq_item(item, indent + 2, out);
            }
        }
        other => out.push(format!("{}{}: {}", pad, key, yaml_scalar(other))),
    }
}

/// Emits the body of a mapping (every key/value pair) at one indent.
fn emit_mapping_body(map: &serde_yaml::Mapping, indent: usize, out: &mut Vec<String>) {
    for (k, v) in map {
        emit_key_value(k.as_str().unwrap_or_default(), v, indent, out);
    }
}

/// Emits a nested value body (mapping or sequence) at one indent.
fn emit_value_body(v: &serde_yaml::Value, indent: usize, out: &mut Vec<String>) {
    match v {
        serde_yaml::Value::Mapping(m) => emit_mapping_body(m, indent, out),
        serde_yaml::Value::Sequence(s) => {
            for item in s {
                emit_seq_item(item, indent, out);
            }
        }
        // Scalars never appear as a bare body, but stay harmless if they do.
        _ => out.push(format!("{}{}", " ".repeat(indent), yaml_scalar(v))),
    }
}

/// Emits one sequence item. A mapping item puts its first key on the `- ` line.
fn emit_seq_item(item: &serde_yaml::Value, indent: usize, out: &mut Vec<String>) {
    let pad = " ".repeat(indent);
    match item {
        serde_yaml::Value::Mapping(m) => {
            let mut iter = m.iter();
            if let Some((k, v)) = iter.next() {
                let key = k.as_str().unwrap_or_default();
                match v {
                    serde_yaml::Value::Mapping(_) | serde_yaml::Value::Sequence(_) => {
                        out.push(format!("{}- {}:", pad, key));
                        emit_value_body(v, indent + 2, out);
                    }
                    other => out.push(format!("{}- {}: {}", pad, key, yaml_scalar(other))),
                }
                for (k2, v2) in iter {
                    emit_key_value(k2.as_str().unwrap_or_default(), v2, indent + 2, out);
                }
            }
        }
        serde_yaml::Value::Sequence(_) => {
            for sub in item.as_sequence().unwrap() {
                emit_seq_item(sub, indent, out);
            }
        }
        other => out.push(format!("{}- {}", pad, yaml_scalar(other))),
    }
}

/// Splices the rendered routes into the raw document, keeping every other line.
pub fn render_routes(raw: &str, entries: &[RouteEntry]) -> Result<String, String> {
    let mut lines: Vec<String> = raw.lines().map(|l| l.to_string()).collect();
    let spot = locate_providers(&lines)?;
    if entries.is_empty() && matches!(spot, Spot::AppendEntry) {
        // Nothing to write and nothing there yet: leave the file alone.
        return Ok(raw.to_string());
    }

    let trailing_newline = raw.ends_with('\n');
    lines.retain(|l| l.trim() != "[]");
    let providers_indent = match &spot {
        Spot::Existing { indent, .. } | Spot::UnderConfig { indent, .. } => *indent,
        _ => 4,
    };

    let mut rendered: Vec<String> = Vec::new();
    for entry in entries {
        rendered.extend(render_route(providers_indent + 2, &entry.id, &entry.map)?);
    }

    match spot {
        Spot::Existing {
            key_line,
            content_end,
            indent,
        } => {
            let mut block = vec![format!(
                "{}providers:{}",
                " ".repeat(indent),
                if entries.is_empty() { " {}" } else { "" }
            )];
            block.extend(rendered);
            lines.splice(key_line..content_end, block);
        }
        Spot::UnderConfig { at, indent } => {
            if entries.is_empty() {
                return Ok(raw.to_string());
            }
            let mut block = vec![format!("{}providers:", " ".repeat(indent))];
            block.extend(rendered);
            lines.splice(at..at, block);
        }
        Spot::UnderEntry { at } => {
            if entries.is_empty() {
                return Ok(raw.to_string());
            }
            let mut block = vec!["  config:".to_string(), "    providers:".to_string()];
            block.extend(rendered);
            lines.splice(at..at, block);
        }
        Spot::AppendEntry => {
            if entries.is_empty() {
                return Ok(raw.to_string());
            }
            while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
                lines.pop();
            }
            let mut block = vec![
                "- id: llm-pi-ai".to_string(),
                format!("  name: \"{PIAI_MODULE}\""),
                "  config:".to_string(),
                "    providers:".to_string(),
            ];
            block.extend(rendered);
            lines.extend(block);
        }
    }

    let mut out = lines.join("\n");
    if trailing_newline || out.is_empty() {
        out.push('\n');
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Model discovery
// ---------------------------------------------------------------------------

/// `listingUrl` in pi-ai: OpenAI flavours append `/models`, Anthropic uses
/// `/v1/models` off the root and drops a trailing `/v1`.
pub fn listing_url(base_url: &str, api: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if api == "anthropic-messages" {
        let root = base.strip_suffix("/v1").unwrap_or(base);
        format!("{root}/v1/models?limit={ANTHROPIC_MODEL_LIMIT}")
    } else {
        format!("{base}/models")
    }
}

pub fn request_headers(api: &str, api_key: Option<&str>) -> Vec<(String, String)> {
    let mut headers = vec![("accept".to_string(), "application/json".to_string())];
    let key = api_key.map(|k| k.trim()).filter(|k| !k.is_empty());
    if api == "anthropic-messages" {
        headers.push(("anthropic-version".to_string(), ANTHROPIC_VERSION.to_string()));
        if let Some(key) = key {
            headers.push(("x-api-key".to_string(), key.to_string()));
        }
    } else if let Some(key) = key {
        headers.push(("authorization".to_string(), format!("Bearer {key}")));
    }
    headers
}

fn dig<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for step in path {
        current = current.get(step)?;
    }
    Some(current)
}

fn capacity(value: &serde_json::Value, paths: &[&[&str]]) -> Option<u64> {
    for path in paths {
        if let Some(found) = dig(value, path).and_then(|v| v.as_u64()) {
            if found > 0 {
                return Some(found);
            }
        }
    }
    None
}

fn first_string(value: &serde_json::Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(text) = value.get(key).and_then(|v| v.as_str()) {
            if !text.trim().is_empty() {
                return text.trim().to_string();
            }
        }
    }
    String::new()
}

/// `readListing` in pi-ai: a `data` array wins, otherwise a `models` object
/// whose values are objects; anything else is reported as unsupported.
pub fn parse_listing(body: &serde_json::Value) -> Result<Vec<CatalogModel>, String> {
    let pairs: Vec<(String, &serde_json::Value)> =
        if let Some(data) = body.get("data").and_then(|v| v.as_array()) {
            data.iter().map(|v| (String::new(), v)).collect()
        } else if let Some(models) = body.get("models").and_then(|v| v.as_object()) {
            models
                .iter()
                .filter(|(_, v)| v.is_object())
                .map(|(k, v)| (k.clone(), v))
                .collect()
        } else {
            return Err(
                "该端点的模型列表既没有 data 数组也没有 models 对象，请手动添加模型。".to_string(),
            );
        };

    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (key, raw) in pairs {
        let id = if key.is_empty() {
            first_string(raw, &["id"])
        } else {
            key
        };
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        let name = first_string(raw, &["name", "display_name", "displayName"]);
        out.push(CatalogModel {
            name: if name.is_empty() { id.clone() } else { name },
            context_window: capacity(
                raw,
                &[
                    &["contextWindow"],
                    &["context_window"],
                    &["context_length"],
                    &["max_input_tokens"],
                    &["limit", "context"],
                ],
            ),
            max_tokens: capacity(
                raw,
                &[
                    &["maxOutputTokens"],
                    &["max_output_tokens"],
                    &["maxTokens"],
                    &["max_tokens"],
                    &["limit", "output"],
                    &["top_provider", "max_completion_tokens"],
                ],
            ),
            id,
            input: Vec::new(),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

fn http_client() -> Result<reqwest::Client, String> {
    crate::proxy::apply(reqwest::Client::builder())
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("dsh-launcher")
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

/// The only function that touches the network; everything above is pure so the
/// test suite stays offline.
async fn fetch_listing(
    client: &reqwest::Client,
    url: &str,
    api: &str,
    api_key: Option<&str>,
) -> Result<Vec<CatalogModel>, String> {
    let mut request = client.get(url);
    for (key, value) in request_headers(api, api_key) {
        request = request.header(key, value);
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| format!("无法访问 {url}"))?;
    let status = response.status();
    if !status.is_success() {
        let mut message = format!("{url} 返回 {status}");
        if status.as_u16() == 401 || status.as_u16() == 403 {
            message.push_str("；请检查 API 密钥");
        }
        return Err(message);
    }
    if let Some(length) = response.content_length() {
        if length > MAX_RESPONSE_BYTES as u64 {
            return Err(format!("{url} 返回的内容超过 {MAX_RESPONSE_BYTES} 字节"));
        }
    }
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("读取 {url} 失败: {e}"))?
    {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(format!("{url} 返回的内容超过 {MAX_RESPONSE_BYTES} 字节"));
        }
    }
    let body: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| format!("{url} 未返回 JSON"))?;
    parse_listing(&body)
}

// ---------------------------------------------------------------------------
// Path / state helpers
// ---------------------------------------------------------------------------

fn version_dir_of(state: &AppState, instance_id: &str) -> Result<(PathBuf, Option<String>), String> {
    let cfg = state.config.lock().unwrap();
    let inst = cfg
        .instances
        .iter()
        .find(|i| i.id == instance_id)
        .ok_or_else(|| "实例不存在".to_string())?;
    let version = cfg
        .versions
        .iter()
        .find(|v| v.id == inst.version_id)
        .ok_or_else(|| "版本不存在".to_string())?;
    Ok((
        crate::wsl::version_fs_path(version),
        version.wsl.clone(),
    ))
}

fn env_overrides_of(state: &AppState, instance_id: Option<&str>) -> BTreeMap<String, String> {
    let Some(instance_id) = instance_id else {
        return BTreeMap::new();
    };
    let cfg = state.config.lock().unwrap();
    cfg.instances
        .iter()
        .find(|i| i.id == instance_id)
        .map(|i| i.env_overrides.clone())
        .unwrap_or_default()
}

fn home_fs_of(state: &AppState, home_id: &str) -> Result<PathBuf, String> {
    let cfg = state.config.lock().unwrap();
    cfg.homes
        .iter()
        .find(|h| h.id == home_id)
        .map(crate::wsl::home_fs_path)
        .ok_or_else(|| "DSH_HOME 不存在".to_string())
}

fn patch_path_of(home: &Path, profile: Option<&str>) -> Result<PathBuf, String> {
    match profile {
        None => Ok(home.join(PATCH_FILENAME)),
        Some(name) => {
            let name = name.trim();
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
    // In place, never tmp+rename: the patch file may be a link managed by
    // `links.rs`, which an atomic replace would break.
    std::fs::write(path, text).map_err(|e| format!("写入 {PATCH_FILENAME} 失败: {e}"))
}

fn catalog_ids_of(state: &AppState, instance_id: &str) -> Vec<String> {
    let Ok((version_dir, _)) = version_dir_of(state, instance_id) else {
        return Vec::new();
    };
    cached_catalog(&version_dir)
        .map(|(_, catalog)| catalog.providers.into_iter().map(|p| p.id).collect())
        .unwrap_or_default()
}

/// Reads the routes of one scope and attaches each credential's descriptor.
fn routes_with_credentials(
    state: &AppState,
    home: &Path,
    instance_id: Option<&str>,
    entries: &[RouteEntry],
    catalog: &[String],
) -> Vec<ProviderRoute> {
    let env = env_overrides_of(state, instance_id);
    let profile = None;
    entries
        .iter()
        .map(|entry| {
            let reference = entry
                .map
                .get("apiKeyEnv")
                .map(yaml_str)
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| derive_key_ref(&entry.id));
            let credential =
                crate::credentials::describe(&reference, &env, home, profile).ok();
            to_wire(entry, catalog, credential)
        })
        .collect()
}

fn deepseek_card(state: &AppState, home: &Path, instance_id: Option<&str>) -> ProviderRoute {
    let env = env_overrides_of(state, instance_id);
    let credential = crate::credentials::describe(DEEPSEEK_REF, &env, home, None).ok();
    ProviderRoute {
        id: DEEPSEEK_ID.to_string(),
        display_name: "DeepSeek".to_string(),
        api_key_env: DEEPSEEK_REF.to_string(),
        base_url: DEEPSEEK_BASE_URL.to_string(),
        catalog: false,
        official: true,
        credential,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// The built-in providers the installed catalogue can supply.
#[tauri::command]
pub async fn list_provider_catalog(
    state: State<'_, AppState>,
    instance_id: String,
) -> Result<ProviderCatalog, String> {
    let (version_dir, distro) = version_dir_of(&state, &instance_id)?;
    if let Some(distro) = distro {
        crate::wsl::ensure_distro_running(&state, &distro).await?;
    }
    let (_, catalog) = crate::wsl::run_blocking(move || cached_catalog(&version_dir)).await??;
    Ok(catalog)
}

/// Model list of a built-in provider, answered by the catalogue — no network.
#[tauri::command]
pub async fn list_catalog_models(
    state: State<'_, AppState>,
    instance_id: String,
    provider_id: String,
) -> Result<Vec<CatalogModel>, String> {
    let (version_dir, distro) = version_dir_of(&state, &instance_id)?;
    if let Some(distro) = distro {
        crate::wsl::ensure_distro_running(&state, &distro).await?;
    }
    let version_dir = version_dir.clone();
    let provider_id = provider_id.clone();
    crate::wsl::run_blocking(move || {
        let (dir, _) = cached_catalog(&version_dir)?;
        catalog_models_at(&dir, &provider_id)
    })
    .await?
}

/// Configured routes of one scope, with the DeepSeek card first — the order
/// DSH's own models page uses.
#[tauri::command]
pub async fn list_provider_routes(
    state: State<'_, AppState>,
    home_id: String,
    profile: Option<String>,
    instance_id: Option<String>,
) -> Result<Vec<ProviderRoute>, String> {
    crate::wsl::ensure_home_running(&state, &home_id).await?;
    let home = home_fs_of(&state, &home_id)?;
    let path = patch_path_of(&home, profile.as_deref())?;
    let raw = crate::wsl::run_blocking(move || read_patch(&path)).await??;
    let entries = read_routes(&raw)?;
    let catalog = match instance_id.as_deref() {
        Some(id) => catalog_ids_of(&state, id),
        None => Vec::new(),
    };
    let mut out = vec![deepseek_card(&state, &home, instance_id.as_deref())];
    out.extend(routes_with_credentials(
        &state,
        &home,
        instance_id.as_deref(),
        &entries,
        &catalog,
    ));
    Ok(out)
}

/// Creates or updates one route, writing the credential first and only then the
/// patch, so a rejected save never touches the configuration.
#[tauri::command]
pub async fn save_provider_route(
    state: State<'_, AppState>,
    home_id: String,
    profile: Option<String>,
    instance_id: Option<String>,
    route: ProviderRoute,
    original_id: Option<String>,
    api_key: String,
) -> Result<Vec<ProviderRoute>, String> {
    crate::wsl::ensure_home_running(&state, &home_id).await?;
    let home = home_fs_of(&state, &home_id)?;
    let path = patch_path_of(&home, profile.as_deref())?;

    // The DeepSeek card is credential-only: its route belongs to
    // `dsh-llm-deepseek-api-key`, which DSH writes (and fills with its own
    // model list), so the launcher never touches that entry.
    if route.official {
        let reference = if route.api_key_env.trim().is_empty() {
            DEEPSEEK_REF.to_string()
        } else {
            route.api_key_env.trim().to_string()
        };
        if reference != DEEPSEEK_REF {
            return Err("DeepSeek 卡片的凭据引用固定为 DEEPSEEK_API_KEY".to_string());
        }
        if !api_key.trim().is_empty() {
            let env = env_overrides_of(&state, instance_id.as_deref());
            let info = crate::credentials::describe(&reference, &env, &home, None)?;
            crate::credentials::ensure_writable(&info)?;
            let store = crate::credentials::store_path(&home);
            crate::wsl::run_blocking(move || crate::credentials::set(&store, &reference, api_key.trim()))
                .await??;
        }
        return list_provider_routes(state, home_id, profile, instance_id).await;
    }

    let raw = {
        let path = path.clone();
        crate::wsl::run_blocking(move || read_patch(&path)).await??
    };
    let mut entries = read_routes(&raw)?;
    let catalog = match instance_id.as_deref() {
        Some(id) => catalog_ids_of(&state, id),
        None => Vec::new(),
    };
    let original = original_id.unwrap_or_default();
    let taken: Vec<String> = entries
        .iter()
        .filter(|e| e.id != original)
        .map(|e| e.id.clone())
        .collect();
    validate_route(&route, &taken, &catalog)?;

    let reference = if route.api_key_env.trim().is_empty() {
        derive_key_ref(&route.id)
    } else {
        route.api_key_env.trim().to_string()
    };
    if !api_key.trim().is_empty() {
        let env = env_overrides_of(&state, instance_id.as_deref());
        let info = crate::credentials::describe(&reference, &env, &home, None)?;
        crate::credentials::ensure_writable(&info)?;
        let store = crate::credentials::store_path(&home);
        let key = api_key.trim().to_string();
        crate::wsl::run_blocking(move || crate::credentials::set(&store, &reference, &key)).await??;
    }

    let existing = entries.iter().find(|e| e.id == original).map(|e| e.map.clone());
    let map = apply_route(&route, existing.as_ref());
    let final_id = if original.is_empty() {
        route.id.clone()
    } else {
        original.clone()
    };
    if let Some(entry) = entries.iter_mut().find(|e| e.id == original) {
        entry.id = final_id.clone();
        entry.map = map;
    } else {
        entries.push(RouteEntry { id: final_id, map });
    }
    let next = render_routes(&raw, &entries)?;
    crate::wsl::run_blocking(move || write_patch(&path, &next)).await??;
    list_provider_routes(state, home_id, profile, instance_id).await
}

/// Deletes one route, dropping its credential when no other route still refers
/// to the same reference.
#[tauri::command]
pub async fn delete_provider_route(
    state: State<'_, AppState>,
    home_id: String,
    profile: Option<String>,
    instance_id: Option<String>,
    id: String,
) -> Result<Vec<ProviderRoute>, String> {
    crate::wsl::ensure_home_running(&state, &home_id).await?;
    let home = home_fs_of(&state, &home_id)?;
    let path = patch_path_of(&home, profile.as_deref())?;

    if id == DEEPSEEK_ID {
        let store = crate::credentials::store_path(&home);
        let reference = DEEPSEEK_REF.to_string();
        crate::wsl::run_blocking(move || crate::credentials::unset(&store, &reference)).await??;
        return list_provider_routes(state, home_id, profile, instance_id).await;
    }

    let raw = {
        let path = path.clone();
        crate::wsl::run_blocking(move || read_patch(&path)).await??
    };
    let mut entries = read_routes(&raw)?;
    let removed = entries
        .iter()
        .find(|e| e.id == id)
        .map(|e| {
            e.map
                .get("apiKeyEnv")
                .map(yaml_str)
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| derive_key_ref(&e.id))
        })
        .ok_or_else(|| "提供方不存在".to_string())?;
    entries.retain(|e| e.id != id);
    let still_used = entries.iter().any(|e| {
        e.map
            .get("apiKeyEnv")
            .map(yaml_str)
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| derive_key_ref(&e.id))
            == removed
    });
    let next = render_routes(&raw, &entries)?;
    crate::wsl::run_blocking(move || write_patch(&path, &next)).await??;
    if !still_used {
        let store = crate::credentials::store_path(&home);
        crate::wsl::run_blocking(move || crate::credentials::unset(&store, &removed)).await??;
    }
    list_provider_routes(state, home_id, profile, instance_id).await
}

/// Asks an endpoint which models it serves. Built-in providers are answered by
/// the catalogue without a request, exactly like DSH.
#[tauri::command]
pub async fn discover_provider_models(
    state: State<'_, AppState>,
    input: DiscoverInput,
) -> Result<Vec<CatalogModel>, String> {
    let (version_dir, distro) = version_dir_of(&state, &input.instance_id)?;
    if let Some(distro) = distro {
        crate::wsl::ensure_distro_running(&state, &distro).await?;
    }

    if let Some(provider) = input.provider.clone() {
        let version_dir = version_dir.clone();
        let provider_id = provider.clone();
        let models = crate::wsl::run_blocking(move || {
            let (dir, _) = cached_catalog(&version_dir)?;
            catalog_models_at(&dir, &provider_id)
        })
        .await??;
        if !models.is_empty() {
            return Ok(models);
        }
    }

    let base_url = input.base_url.trim().to_string();
    if base_url.is_empty() {
        return Err("自定义提供方需要先填写 API 地址。".to_string());
    }
    validate_base_url(&base_url)?;
    let api = if input.api.trim().is_empty() {
        "openai-completions".to_string()
    } else {
        input.api.trim().to_string()
    };
    if !LISTABLE_PROTOCOLS.contains(&api.as_str()) {
        return Err("pi-ai 协议不支持读取模型列表，请手动添加模型。".to_string());
    }
    if !input.api_key.trim().is_empty() && !crate::credentials::is_valid_secret(input.api_key.trim())
    {
        return Err("该 API 密钥格式错误，请检查。".to_string());
    }

    // A saved provider probes with its stored key, as DSH does.
    let mut key = input.api_key.trim().to_string();
    if key.is_empty() {
        if let Some(route_id) = input.route_id.as_deref() {
            crate::wsl::ensure_home_running(&state, &input.home_id).await?;
            let home = home_fs_of(&state, &input.home_id)?;
            let path = patch_path_of(&home, input.profile.as_deref())?;
            let raw = crate::wsl::run_blocking(move || read_patch(&path)).await??;
            let entries = read_routes(&raw)?;
            if let Some(entry) = entries.iter().find(|e| e.id == route_id) {
                let reference = entry
                    .map
                    .get("apiKeyEnv")
                    .map(yaml_str)
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| derive_key_ref(&entry.id));
                let store = crate::credentials::store_path(&home);
                let raw_store = crate::wsl::run_blocking(move || {
                    std::fs::read_to_string(&store).unwrap_or_default()
                })
                .await?;
                key = crate::credentials::stored_value(&raw_store, &reference).unwrap_or_default();
            }
        }
    }

    let url = listing_url(&base_url, &api);
    let client = http_client()?;
    let probe_key = if key.is_empty() { None } else { Some(key.as_str()) };
    fetch_listing(&client, &url, &api, probe_key).await
}

/// Descriptor for one credential reference; never returns the secret.
#[tauri::command]
pub async fn describe_credential(
    state: State<'_, AppState>,
    instance_id: Option<String>,
    home_id: String,
    reference: String,
) -> Result<CredentialInfo, String> {
    crate::wsl::ensure_home_running(&state, &home_id).await?;
    let home = home_fs_of(&state, &home_id)?;
    let env = env_overrides_of(&state, instance_id.as_deref());
    crate::wsl::run_blocking(move || {
        crate::credentials::describe(&reference, &env, &home, None)
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_PATCH: &str = r#"# Your patch layer for this dsh profile, applied after every bundle layer:
- id: ui-settings-general
  name: "@deepseek-ai/dsh-client-ui-settings-general"
  config:
    welcomeNoticeVersion: 2026-08-13.1
# 手写注释：不要动这一块
- id: llm-pi-ai
  name: "@deepseek-ai/dsh-llm-pi-ai"
  config:
    providers:
      my-gateway:
        apiKeyEnv: GATEWAY_API_KEY
        api: openai-completions
        baseURL: https://gateway.example/v1
        compat:
          supportsDeveloperRole: false
        headers:
          X-Trace: '1'
        timeoutMs: 60000
        retryPolicy:
          maxAttempts: 3
        modelOverrides:
          legacy-chat:
            input: [text]
        models:
          - id: deepseek-v4.1-flash
            name: DeepSeek V4.1 Flash
            contextWindow: 262144
            maxTokens: 65536
            input: [text, image]
            compat:
              thinkingFormat: deepseek
      moonshotai:
        apiKeyEnv: MOONSHOTAI_API_KEY
"#;

    #[test]
    fn parses_routes_from_the_sample_patch() {
        let entries = read_routes(SAMPLE_PATCH).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "my-gateway");
        assert_eq!(entries[1].id, "moonshotai");

        let route = to_wire(&entries[0], &[], None);
        assert_eq!(route.id, "my-gateway");
        assert_eq!(route.api, "openai-completions");
        assert_eq!(route.base_url, "https://gateway.example/v1");
        assert_eq!(route.models.len(), 1);
        assert_eq!(route.models[0].context_window, Some(262144));
        assert_eq!(route.extra_keys, vec!["compat", "headers", "timeoutMs", "retryPolicy", "modelOverrides"]);
    }

    #[test]
    fn sibling_entries_are_untouched_by_a_save() {
        let entries = read_routes(SAMPLE_PATCH).unwrap();
        let mut next = entries.clone();
        next[0].map.insert(
            serde_yaml::Value::String("displayName".to_string()),
            serde_yaml::Value::String("网关".to_string()),
        );
        let out = render_routes(SAMPLE_PATCH, &next).unwrap();
        assert!(out.contains("# 手写注释：不要动这一块"));
        assert!(out.contains("welcomeNoticeVersion: 2026-08-13.1"));
        assert!(out.contains("- id: ui-settings-general"));
    }

    #[test]
    fn saving_preserves_unmanaged_route_and_model_keys() {
        let entries = read_routes(SAMPLE_PATCH).unwrap();
        let mut next = entries.clone();
        let mut edited = to_wire(&entries[0], &[], None);
        edited.display_name = "我的网关".to_string();
        next[0].map = apply_route(&edited, Some(&entries[0].map));

        let out = render_routes(SAMPLE_PATCH, &next).unwrap();
        for needle in [
            "timeoutMs: 60000",
            "maxAttempts: 3",
            "X-Trace",
            "modelOverrides:",
            "legacy-chat:",
            "input:",
            "compat:",
            "thinkingFormat: deepseek",
            "supportsDeveloperRole: false",
        ] {
            assert!(out.contains(needle), "丢失 {needle}\n{out}");
        }
        assert!(out.contains("displayName: 我的网关"));

        let reparsed = read_routes(&out).unwrap();
        let route = reparsed.iter().find(|e| e.id == "my-gateway").unwrap();
        assert_eq!(
            route.map.get("timeoutMs").and_then(|v| v.as_u64()),
            Some(60000)
        );
        assert!(route.map.get("modelOverrides").is_some());
        let models = route.map.get("models").unwrap().as_sequence().unwrap();
        let first = models[0].as_mapping().unwrap();
        assert!(first.get("input").is_some(), "模型 input 丢失");
        assert!(first.get("compat").is_some(), "模型 compat 丢失");
    }

    #[test]
    fn saving_a_catalog_route_never_invents_api_or_base_url() {
        let entries = read_routes(SAMPLE_PATCH).unwrap();
        let mut next = entries.clone();
        let mut edited = to_wire(&entries[1], &["moonshotai".to_string()], None);
        edited.models.clear();
        next[1].map = apply_route(&edited, Some(&entries[1].map));
        let out = render_routes(SAMPLE_PATCH, &next).unwrap();
        let reparsed = read_routes(&out).unwrap();
        let route = reparsed.iter().find(|e| e.id == "moonshotai").unwrap();
        assert!(route.map.get("api").is_none());
        assert!(route.map.get("baseURL").is_none());
        assert!(route.map.get("models").is_none());
        assert_eq!(
            route.map.get("apiKeyEnv").map(yaml_str).as_deref(),
            Some("MOONSHOTAI_API_KEY")
        );
    }

    #[test]
    fn saving_creates_the_entry_when_absent() {
        let raw = "# header\n[]\n";
        let entries = vec![RouteEntry {
            id: "my-gateway".to_string(),
            map: apply_route(
                &ProviderRoute {
                    id: "my-gateway".to_string(),
                    api: "openai-completions".to_string(),
                    base_url: "https://gateway.example/v1".to_string(),
                    models: vec![ProviderModel {
                        id: "deepseek-v4.1-flash".to_string(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                None,
            ),
        }];
        let out = render_routes(raw, &entries).unwrap();
        assert!(out.contains("# header"));
        assert!(!out.contains("[]"));
        assert!(out.contains("- id: llm-pi-ai"));
        assert!(out.contains("name: \"@deepseek-ai/dsh-llm-pi-ai\""));
        let reparsed = read_routes(&out).unwrap();
        assert_eq!(reparsed.len(), 1);
        assert_eq!(reparsed[0].id, "my-gateway");
    }

    #[test]
    fn saving_an_empty_list_keeps_the_dormant_dict() {
        let out = render_routes(SAMPLE_PATCH, &[]).unwrap();
        assert!(out.contains("providers: {}"));
        assert!(out.contains("- id: llm-pi-ai"));
        assert!(out.contains("# 手写注释：不要动这一块"));
    }

    #[test]
    fn rendering_leaves_a_document_without_routes_alone() {
        let raw = "# header\n- id: ui-settings-general\n  name: \"x\"\n";
        assert_eq!(render_routes(raw, &[]).unwrap(), raw);
    }

    #[test]
    fn provider_id_is_permanent() {
        let entries = read_routes(SAMPLE_PATCH).unwrap();
        let mut next = entries.clone();
        let mut edited = to_wire(&entries[0], &[], None);
        edited.id = "renamed".to_string();
        next[0].map = apply_route(&edited, Some(&entries[0].map));
        let out = render_routes(SAMPLE_PATCH, &next).unwrap();
        assert!(out.contains("my-gateway:"), "编辑不应改名:\n{out}");
    }

    #[test]
    fn validate_route_rules() {
        let catalog = vec!["moonshotai".to_string()];
        let ok = ProviderRoute {
            id: "moonshotai".to_string(),
            ..Default::default()
        };
        assert!(validate_route(&ok, &[], &catalog).is_ok());

        let dup = ProviderRoute {
            id: "moonshotai".to_string(),
            ..Default::default()
        };
        assert!(validate_route(&dup, &["moonshotai".to_string()], &catalog).is_err());

        let bad_id = ProviderRoute {
            id: "My Gateway".to_string(),
            ..Default::default()
        };
        assert!(validate_route(&bad_id, &[], &catalog).is_err());

        let no_url = ProviderRoute {
            id: "my-gateway".to_string(),
            api: "openai-completions".to_string(),
            models: vec![ProviderModel {
                id: "m".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(validate_route(&no_url, &[], &catalog).is_err());

        let no_models = ProviderRoute {
            id: "my-gateway".to_string(),
            api: "openai-completions".to_string(),
            base_url: "https://gateway.example/v1".to_string(),
            ..Default::default()
        };
        assert!(validate_route(&no_models, &[], &catalog).is_err());

        let no_api = ProviderRoute {
            id: "my-gateway".to_string(),
            base_url: "https://gateway.example/v1".to_string(),
            models: vec![ProviderModel {
                id: "m".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(validate_route(&no_api, &[], &catalog).is_err());

        let valid_custom = ProviderRoute {
            id: "my-gateway".to_string(),
            api: "openai-completions".to_string(),
            base_url: "https://gateway.example/v1".to_string(),
            models: vec![ProviderModel {
                id: "m".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(validate_route(&valid_custom, &[], &catalog).is_ok());
    }

    #[test]
    fn derive_key_ref_matches_dsh() {
        assert_eq!(derive_key_ref("moonshotai"), "MOONSHOTAI_API_KEY");
        assert_eq!(derive_key_ref("minimax-cn"), "MINIMAX_CN_API_KEY");
        assert_eq!(derive_key_ref("my-gateway"), "MY_GATEWAY_API_KEY");
        assert_eq!(derive_key_ref("zai"), "ZAI_API_KEY");
    }

    #[test]
    fn listing_url_openai_and_anthropic() {
        assert_eq!(
            listing_url("https://g.example/v1/", "openai-completions"),
            "https://g.example/v1/models"
        );
        assert_eq!(
            listing_url("https://g.example/v1", "anthropic-messages"),
            "https://g.example/v1/models?limit=1000"
        );
        assert_eq!(
            listing_url("https://g.example", "anthropic-messages"),
            "https://g.example/v1/models?limit=1000"
        );
    }

    #[test]
    fn request_headers_per_protocol() {
        let anthropic = request_headers("anthropic-messages", Some("k"));
        assert!(anthropic.iter().any(|(k, v)| k == "x-api-key" && v == "k"));
        assert!(anthropic
            .iter()
            .any(|(k, _)| k == "anthropic-version"));

        let openai = request_headers("openai-completions", Some("k"));
        assert!(openai
            .iter()
            .any(|(k, v)| k == "authorization" && v == "Bearer k"));

        let anonymous = request_headers("openai-completions", None);
        assert!(!anonymous.iter().any(|(k, _)| k == "authorization"));
        assert!(anonymous.iter().any(|(k, _)| k == "accept"));
    }

    #[test]
    fn parse_listing_prefers_the_data_array() {
        let body: serde_json::Value = serde_json::from_str(
            r#"{"data":[{"id":"a","name":"A","context_window":128000,"max_output_tokens":4096},{"id":"b"}]}"#,
        )
        .unwrap();
        let models = parse_listing(&body).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "a");
        assert_eq!(models[0].name, "A");
        assert_eq!(models[0].context_window, Some(128000));
        assert_eq!(models[0].max_tokens, Some(4096));
        assert_eq!(models[1].name, "b");
    }

    #[test]
    fn parse_listing_reads_a_models_object() {
        let body: serde_json::Value =
            serde_json::from_str(r#"{"models":{"x":{"displayName":"X"},"y":"skip"}}"#).unwrap();
        let models = parse_listing(&body).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "x");
        assert_eq!(models[0].name, "X");
    }

    #[test]
    fn parse_listing_reports_an_unknown_shape() {
        let body: serde_json::Value = serde_json::from_str(r#"{"foo":1}"#).unwrap();
        let err = parse_listing(&body).unwrap_err();
        assert!(err.contains("既没有 data 数组也没有 models 对象"), "{err}");
    }

    #[test]
    fn parse_listing_skips_entries_without_an_id() {
        let body: serde_json::Value =
            serde_json::from_str(r#"{"data":[{"name":"no-id"},{"id":"ok"}]}"#).unwrap();
        let models = parse_listing(&body).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "ok");
    }

    #[test]
    fn catalog_is_read_from_a_pnpm_layout() {
        let dir = std::env::temp_dir().join(format!("dsh-launcher-cat-{}", uuid::Uuid::new_v4()));
        let package = dir
            .join("node_modules")
            .join(".pnpm")
            .join("@earendil-works+pi-ai@0.87.1_ws@8.22.0_zod@4.6.5")
            .join("node_modules")
            .join("@earendil-works")
            .join("pi-ai");
        let data = package.join("dist").join("providers").join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(
            data.join("moonshotai.json"),
            r#"{"openai-completions":{"kimi-k2.6":{"id":"kimi-k2.6","name":"Kimi K2.6","baseUrl":"https://api.moonshot.ai/v1","contextWindow":262144,"maxTokens":262144,"input":["text","image"]}}}"#,
        )
        .unwrap();
        std::fs::write(
            package.join("dist").join("providers").join("moonshotai.js"),
            "export function moonshotaiProvider() {\n  return createProvider({\n    id: \"moonshotai\",\n    name: \"Moonshot AI\",\n    auth: {\n      apiKey: moonshotaiApiKeyAuth(),\n    },\n  });\n}",
        )
        .unwrap();

        let (found, catalog) = load_catalog_at(&dir).unwrap();
        assert_eq!(found, package);
        assert_eq!(catalog.providers.len(), 1);
        assert_eq!(catalog.providers[0].id, "moonshotai");
        assert_eq!(catalog.providers[0].name, "Moonshot AI");
        assert_eq!(catalog.providers[0].api, "openai-completions");
        assert_eq!(catalog.providers[0].base_url, "https://api.moonshot.ai/v1");
        assert!(catalog.providers[0].api_key);

        let models = catalog_models_at(&found, "moonshotai").unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "kimi-k2.6");
        assert_eq!(models[0].input, vec!["text".to_string(), "image".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_missing_is_an_error_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("dsh-launcher-cat-missing-{}", uuid::Uuid::new_v4()));
        let err = load_catalog_at(&dir).unwrap_err();
        assert!(err.contains("@earendil-works/pi-ai"), "{err}");
    }

    #[test]
    fn oauth_only_providers_are_flagged() {
        let dir = std::env::temp_dir().join(format!("dsh-launcher-cat-oauth-{}", uuid::Uuid::new_v4()));
        let package = dir.join("node_modules").join("@earendil-works").join("pi-ai");
        let data = package.join("dist").join("providers").join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(
            data.join("openai-codex.json"),
            r#"{"openai-codex-responses":{"gpt":{"id":"gpt","baseUrl":"https://x"}}}"#,
        )
        .unwrap();
        std::fs::write(
            package.join("dist").join("providers").join("openai-codex.js"),
            r#"export function openaiCodexProvider(){ return { id: "openai-codex", name: "Codex", auth: [{ id: "oauth" }] } }"#,
        )
        .unwrap();
        let (_, catalog) = load_catalog_at(&dir).unwrap();
        assert!(!catalog.providers[0].api_key);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
