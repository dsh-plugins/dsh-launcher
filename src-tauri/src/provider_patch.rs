//! Provider 路由在 DSH patch 层中的读写 (Issue #76)。
//!
//! DSH 从 `@deepseek-ai/dsh-llm-pi-ai` 这个 loader 条目的 `config.providers`
//! 读取多供应商路由，而这些条目位于 patch 层文件中：
//!
//! * 全局 -> `<DSH_HOME>/cordis.patch.yml`
//! * profile -> `<DSH_HOME>/profiles/<profile>/cordis.patch.yml`
//!
//! patch 文件是一个顶层 YAML 数组，可能同时携带 bundle insert、手写的 id 覆盖
//! 和 `!!js` 标量，因此保存时**绝不整体重新序列化文档**：本模块管理的 pi-ai
//! 条目由标记注释界定，保存时先整段摘除、再整段追加，其余每一行都按字节保留。
//! 读取则相反——解析整个文档，这样标记块之外手写的路由也能被列出。
//!
//! 注意与 [`crate::mcp`] 的区别：MCP 把多台服务器写进**同一个** insert 块的多行，
//! 这里则是**一条** pi-ai 条目、其 `config.providers` 是一个字典。

use std::path::{Path, PathBuf};

use crate::provider_config::ProviderRoute;

/// 承载供应商路由的 loader 模块名。
pub const PI_AI_MODULE: &str = "@deepseek-ai/dsh-llm-pi-ai";
/// patch 层文件名。
pub const PATCH_FILENAME: &str = "cordis.patch.yml";
/// 界定本模块所管理条目的注释标记。
const BLOCK_BEGIN: &str = "# dsh-launcher providers begin";
const BLOCK_END: &str = "# dsh-launcher providers end";

// ---------------------------------------------------------------------------
// 路径
// ---------------------------------------------------------------------------

/// patch 路径：`None` = DSH_HOME 本身（全局），`Some(profile)` = 该 profile 目录。
pub fn patch_path(home: &Path, profile: Option<&str>) -> Result<PathBuf, String> {
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

pub fn read_patch(path: &Path) -> Result<String, String> {
    if !path.exists() {
        return Ok(String::new());
    }
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("读取 {PATCH_FILENAME} 失败: {e}"))?;
    // Windows editors (Notepad, PowerShell `Set-Content`) often prepend a UTF-8
    // BOM. `str::trim` does not strip it, which would make `[]` unrecognisable
    // and produce a two-document file. Drop it once, here.
    Ok(text
        .strip_prefix('\u{feff}')
        .map(str::to_string)
        .unwrap_or(text))
}

pub fn write_patch(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    std::fs::write(path, text).map_err(|e| format!("写入 {PATCH_FILENAME} 失败: {e}"))
}

// ---------------------------------------------------------------------------
// 读取：从 patch 文档中解析出路由
// ---------------------------------------------------------------------------

/// 列出 patch 文档中 pi-ai 条目声明的全部路由，按字典顺序。
///
/// 同时覆盖标记块之外手写的 pi-ai 条目。
pub fn parse_providers(raw: &str) -> Result<Vec<ProviderRoute>, String> {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let doc: serde_yaml::Value =
        serde_yaml::from_str(raw).map_err(|e| format!("解析 {PATCH_FILENAME} 失败: {e}"))?;
    let Some(entries) = doc.as_sequence() else {
        return Ok(Vec::new());
    };

    let mut routes: Vec<ProviderRoute> = Vec::new();
    for entry in entries {
        let Some(map) = entry.as_mapping() else {
            continue;
        };
        if map.get(ykey("name")).and_then(|v| v.as_str()) != Some(PI_AI_MODULE) {
            continue;
        }
        let Some(providers) = map
            .get(ykey("config"))
            .and_then(|c| c.as_mapping())
            .and_then(|c| c.get(ykey("providers")))
            .and_then(|p| p.as_mapping())
        else {
            continue;
        };
        for (key, value) in providers {
            let Some(name) = key.as_str() else {
                continue;
            };
            // A malformed value (e.g. a scalar instead of a mapping) must not
            // make the whole file unreadable: skip it and keep the rest.
            let Some(mut route) = yaml_to_route(name, value) else {
                continue;
            };
            // The dict key is authoritative for the route name.
            route.name = name.to_string();
            routes.push(route);
        }
    }
    Ok(routes)
}

/// Maps one `config.providers` value to a route, or `None` if it is not a
/// well-formed route mapping.
fn yaml_to_route(name: &str, value: &serde_yaml::Value) -> Option<ProviderRoute> {
    let json = serde_json::to_value(value).ok()?;
    let mut route: ProviderRoute = serde_json::from_value(json).ok()?;
    route.name = name.to_string();
    Some(route)
}

/// 把一条路由转成 `config.providers` 字典中的值（不含 name 键）。
fn route_to_value(route: &ProviderRoute) -> Result<serde_yaml::Value, String> {
    let mut json = serde_json::to_value(route)
        .map_err(|e| format!("序列化 provider 路由 '{}' 失败: {e}", route.name))?;
    if let Some(map) = json.as_object_mut() {
        // `name` is the dict key, never a field of the value.
        map.remove("name");
    }
    serde_yaml::to_value(json).map_err(|e| format!("转换 provider 路由失败: {e}"))
}

// ---------------------------------------------------------------------------
// 写入：摘除 / 追加本模块管理的 pi-ai 条目
// ---------------------------------------------------------------------------

/// 摘除本模块管理的 pi-ai 条目（连同标记注释），其余每一行按字节保留。
///
/// 只移除由标记界定、或 `- name: '@deepseek-ai/dsh-llm-pi-ai'` 开头的顶层条目；
/// 手写在别处的 pi-ai 条目**不**在此摘除（它们由 `render_providers` 追加回去时
/// 会与原条目共存——这是有意的：本模块只拥有自己写下的那一条）。
///
/// 标记必须顶格（第 0 列）；缩进的同名注释不会被当作标记，避免误删文件。
pub fn strip_provider_entry(raw: &str) -> String {
    let lines: Vec<&str> = raw.lines().collect();
    let mut dropped = vec![false; lines.len()];
    let mut i = 0;

    while i < lines.len() {
        let trimmed = lines[i].trim();

        // 标记块：仅在能找到配对的结束标记时整块摘除。找不到配对说明文件被
        // 外部破坏，此时宁可什么都不删——绝不能把用户其余内容一并吞掉。
        // 标记必须顶格（第 0 列）；缩进的同名注释不会被当作标记，避免误删文件。
        if trimmed == BLOCK_BEGIN && indent_of(lines[i]) == 0 {
            let end = (i + 1..lines.len())
                .find(|&j| lines[j].trim() == BLOCK_END && indent_of(lines[j]) == 0);
            match end {
                Some(j) => {
                    for flag in dropped.iter_mut().take(j + 1).skip(i) {
                        *flag = true;
                    }
                    i = j + 1;
                }
                None => i += 1,
            }
            continue;
        }

        // 无标记的 pi-ai 条目：`- name: '@deepseek-ai/dsh-llm-pi-ai'` 起的条目，
        // 也包括挂在 `insert:` 下的嵌套形式。
        if let Some(entry) = pi_ai_entry_start(lines[i]) {
            if let Some(end) = entry_end(&lines, i, entry) {
                for flag in dropped.iter_mut().take(end).skip(i) {
                    *flag = true;
                }
                i = end;
                continue;
            }
        }

        i += 1;
    }

    let kept: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(index, _)| !dropped[*index])
        .map(|(_, line)| *line)
        .collect();
    // Preserve the file's own line ending so a CRLF file keeps CRLF.
    let eol = if raw.contains("\r\n") { "\r\n" } else { "\n" };
    let mut text = kept.join(eol);
    if !text.is_empty() {
        text.push_str(eol);
    }
    text
}

/// 一个 pi-ai 条目的起始信息：它的缩进、以及它挂在哪一级列表下。
struct EntryStart {
    /// 列表项标记符（`- `）所在行的缩进。
    marker_indent: usize,
}

/// 判断某一行是否**开始**一个 pi-ai 条目。
///
/// 覆盖两种写法：
/// * 顶层裸条目 `- name: '@deepseek-ai/dsh-llm-pi-ai'`
/// * 挂在 `insert:` 下的 `  - name: '@deepseek-ai/dsh-llm-pi-ai'`
///
/// 两种情况下，条目自身的键都从 `- ` 标记之后开始；后续键与其同级缩进对齐。
fn pi_ai_entry_start(line: &str) -> Option<EntryStart> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("- ")?;
    let rest = rest.trim();
    let value = rest.strip_prefix("name:")?.trim();
    let value = value.trim_matches(|c| c == '\'' || c == '"');
    if value != PI_AI_MODULE {
        return None;
    }
    Some(EntryStart {
        marker_indent: indent_of(line),
    })
}

/// 求 pi-ai 条目的结束行号（不含）。条目键与 `- ` 标记缩进对齐，因此结束于
/// 下一个缩进 **<= 标记缩进**、且非空的、本身是 `- ` 新项或更浅缩进的行。
fn entry_end(lines: &[&str], start: usize, entry: EntryStart) -> Option<usize> {
    let indent = entry.marker_indent;
    let mut j = start + 1;
    while j < lines.len() {
        let line = lines[j];
        if line.trim().is_empty() {
            j += 1;
            continue;
        }
        let cur = indent_of(line);
        if cur <= indent {
            // 同级或更浅：新项或条目已结束。
            break;
        }
        j += 1;
    }
    Some(j)
}

fn indent_of(line: &str) -> usize {
    // 以字符计缩进，避免多字节字符影响列数判断。
    line.len() - line.trim_start().len()
}

fn ykey(key: &str) -> serde_yaml::Value {
    serde_yaml::Value::String(key.to_string())
}

/// Whether a patch document carries no entry (only comments / blank lines / `[]`).
fn is_document_empty(text: &str) -> bool {
    text.lines()
        .map(|line| line.trim().trim_start_matches('\u{feff}').trim())
        .all(|line| line.is_empty() || line.starts_with('#') || line == "[]")
}

/// 渲染本模块管理的 pi-ai 条目块，使用给定的行尾符。
fn render_block_eol(routes: &[ProviderRoute], eol: &str) -> Result<String, String> {
    let mut providers = serde_yaml::Mapping::new();
    for route in routes {
        providers.insert(ykey(&route.name), route_to_value(route)?);
    }

    let mut config = serde_yaml::Mapping::new();
    config.insert(ykey("providers"), serde_yaml::Value::Mapping(providers));

    let mut entry = serde_yaml::Mapping::new();
    entry.insert(ykey("name"), ykey(PI_AI_MODULE));
    entry.insert(ykey("config"), serde_yaml::Value::Mapping(config));

    let text = serde_yaml::to_string(&serde_yaml::Value::Mapping(entry))
        .map_err(|e| format!("序列化 provider 配置失败: {e}"))?;

    let mut out = String::new();
    out.push_str(BLOCK_BEGIN);
    out.push_str(eol);
    let mut first = true;
    for line in text.lines() {
        // Only the leading document marker is stripped; interior blank lines and
        // `---`/`...` that belong to a block scalar must survive verbatim.
        if first && (line.trim().is_empty() || line == "---" || line == "...") {
            continue;
        }
        out.push_str(if first { "- " } else { "  " });
        out.push_str(line);
        out.push_str(eol);
        first = false;
    }
    out.push_str(BLOCK_END);
    out.push_str(eol);
    Ok(out)
}

/// 重写 `raw`，使其管理块只包含 `routes`。其余行按字节保留。
///
/// 空列表时若文档已没有其他内容，则落回 `[]` 占位符。
pub fn render_providers(raw: &str, routes: &[ProviderRoute]) -> Result<String, String> {
    let eol = if raw.contains("\r\n") { "\r\n" } else { "\n" };
    let stripped = strip_provider_entry(raw);
    let mut lines: Vec<&str> = stripped.lines().collect();

    if !routes.is_empty() {
        // 空文档的 `[]` 占位符不能与块序列共存（那是两个 YAML 文档）。
        // `trim` 不会去掉 U+FEFF，因此显式剥掉 BOM 再比较。
        lines.retain(|line| line.trim().trim_start_matches('\u{feff}').trim() != "[]");
    }
    while lines.last().map(|line| line.trim().is_empty()) == Some(true) {
        lines.pop();
    }

    let mut out = lines.join(eol);
    if !out.is_empty() {
        out.push_str(eol);
    }

    if routes.is_empty() {
        if is_document_empty(&out) && !out.contains("[]") {
            out.push_str("[]");
            out.push_str(eol);
        }
        return Ok(out);
    }

    out.push_str(&render_block_eol(routes, eol)?);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_config::ProviderModel;

    /// The production path goes YAML -> serde_json -> ProviderRoute, which is
    /// not covered by the direct `serde_yaml` round-trip test in provider_config.
    #[test]
    fn test_production_path_keeps_unknown_keys_and_reasoning() {
        let raw = "# keep me\n- name: '@deepseek-ai/dsh-llm-pi-ai'\n  config:\n    providers:\n      acme:\n        apiKeyEnv: K\n        api: openai-completions\n        baseURL: https://x/v1\n        compat:\n          thinkingFormat: deepseek\n        models:\n          - id: m1\n            reasoningEfforts:\n              off:\n              high: high\n";
        let routes = parse_providers(raw).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].name, "acme");
        assert!(
            routes[0].extra.contains_key("compat"),
            "compat lost: {:?}",
            routes[0].extra
        );
        assert!(
            routes[0].models[0].reasoning_efforts.is_some(),
            "reasoningEfforts lost"
        );
        let out = render_providers(raw, &routes).unwrap();
        assert!(out.contains("thinkingFormat"), "{out}");
        assert!(out.contains("high: high"), "{out}");
        assert!(out.contains("# keep me"), "{out}");
    }

    fn route(name: &str) -> ProviderRoute {
        ProviderRoute {
            name: name.to_string(),
            api_key_env: Some("TEST_API_KEY".to_string()),
            api: Some("openai-completions".to_string()),
            base_url: Some("https://api.test.example/v1".to_string()),
            models: vec![ProviderModel {
                id: "test-model".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn test_patch_path_scopes() {
        let home = Path::new("C:\\h");
        assert_eq!(
            patch_path(home, None).unwrap(),
            home.join("cordis.patch.yml")
        );
        assert_eq!(
            patch_path(home, Some("main")).unwrap(),
            home.join("profiles").join("main").join("cordis.patch.yml")
        );
        assert!(patch_path(home, Some("")).is_err());
        assert!(patch_path(home, Some("../evil")).is_err());
    }

    #[test]
    fn test_render_into_empty_document_uses_sequence() {
        let out = render_providers("[]\n", &[route("acme")]).unwrap();
        assert!(out.contains("dsh-launcher providers begin"), "{out}");
        assert!(out.contains(PI_AI_MODULE), "{out}");
        assert!(out.contains("acme:"), "{out}");
        // The `[]` placeholder is gone.
        assert!(!out.lines().any(|l| l.trim() == "[]"), "{out}");
    }

    #[test]
    fn test_round_trip() {
        let routed = vec![route("acme"), route("other")];
        let text = render_providers("[]\n", &routed).unwrap();
        let parsed = parse_providers(&text).unwrap();
        let mut names: Vec<&str> = parsed.iter().map(|r| r.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["acme", "other"]);
        assert_eq!(parsed[0].models[0].id, "test-model");
    }

    #[test]
    fn test_foreign_lines_preserved_byte_for_byte() {
        let raw = "# top comment\n- name: '@deepseek-ai/other-plugin'\n  config:\n    x: !!js process.env.FOO\n[]\n";
        let out = render_providers(raw, &[route("acme")]).unwrap();
        assert!(out.contains("# top comment"), "{out}");
        assert!(out.contains("@deepseek-ai/other-plugin"), "{out}");
        assert!(out.contains("!!js process.env.FOO"), "{out}");
        assert!(out.contains("acme:"), "{out}");
    }

    #[test]
    fn test_render_overwrites_previous_managed_block() {
        let first = render_providers("[]\n", &[route("acme")]).unwrap();
        let second = render_providers(&first, &[route("beta")]).unwrap();
        assert!(second.contains("beta:"), "{second}");
        assert!(!second.contains("acme:"), "{second}");
        assert_eq!(second.matches(BLOCK_BEGIN).count(), 1, "{second}");
    }

    #[test]
    fn test_remove_all_routes_restores_empty_placeholder() {
        let with = render_providers("[]\n", &[route("acme")]).unwrap();
        let empty = render_providers(&with, &[]).unwrap();
        assert_eq!(empty.trim(), "[]");
    }

    #[test]
    fn test_strip_only_removes_managed_entry() {
        let raw = "- name: '@deepseek-ai/other-plugin'\n  config:\n    a: 1\n- name: '@deepseek-ai/dsh-llm-pi-ai'\n  config:\n    providers: {}\n";
        let out = strip_provider_entry(raw);
        assert!(out.contains("@deepseek-ai/other-plugin"), "{out}");
        assert!(!out.contains("dsh-llm-pi-ai"), "{out}");
    }

    /// A hand-corrupted file with an unclosed `begin` marker must not lose the
    /// user's content: the strip stops at the end of the document but never
    /// deletes entries it cannot prove are ours beyond the marker.
    #[test]
    fn test_unclosed_marker_does_not_destroy_following_content() {
        let raw = "# dsh-launcher providers begin\n- name: '@deepseek-ai/dsh-llm-pi-ai'\n  config:\n    providers: {}\n- name: '@deepseek-ai/keep-me'\n  config:\n    a: 1\n";
        let out = strip_provider_entry(raw);
        assert!(out.contains("keep-me"), "destroyed user content: {out:?}");
        assert!(
            out.contains("dsh-launcher providers begin"),
            "marker left: {out:?}"
        );
    }

    /// An indented comment that merely looks like a marker is not a marker.
    #[test]
    fn test_indented_marker_is_not_a_marker() {
        let raw = "  # dsh-launcher providers begin\n- name: '@deepseek-ai/keep-me'\n  config:\n    a: 1\n";
        let out = strip_provider_entry(raw);
        assert!(out.contains("keep-me"), "{out:?}");
        assert!(out.contains("providers begin"), "{out:?}");
    }

    /// A UTF-8 BOM before `[]` must not defeat the placeholder removal, or the
    /// result is two YAML documents and DSH can no longer read the file.
    #[test]
    fn test_bom_before_placeholder_is_handled() {
        let raw = "\u{feff}[]\n";
        let out = render_providers(raw, &[route("acme")]).unwrap();
        assert!(!out.contains("[]"), "{out:?}");
        // And the result parses back.
        assert_eq!(parse_providers(&out).unwrap().len(), 1);
    }

    /// A pi-ai entry nested under `insert:` (the shape a bundle install writes)
    /// must be recognised and removed, not left to duplicate.
    #[test]
    fn test_nested_insert_pi_ai_entry_is_stripped() {
        let raw = "- insert:\n  - name: '@deepseek-ai/dsh-llm-pi-ai'\n    config:\n      providers:\n        stale:\n          api: openai-completions\n- name: '@deepseek-ai/other'\n  config:\n    x: 1\n";
        let out = render_providers(raw, &[route("fresh")]).unwrap();
        assert!(!out.contains("stale"), "stale entry survived: {out:?}");
        assert!(out.contains("@deepseek-ai/other"), "{out:?}");
        assert!(out.contains("fresh"), "{out:?}");
        // Exactly one pi-ai entry remains.
        assert_eq!(out.matches(PI_AI_MODULE).count(), 1, "{out:?}");
    }

    /// A CRLF file must keep CRLF: the launcher must not rewrite every foreign
    /// line's terminator.
    #[test]
    fn test_crlf_is_preserved() {
        let raw = "# keep\r\n- name: '@deepseek-ai/other'\r\n  config:\r\n    x: 1\r\n";
        let out = render_providers(raw, &[route("acme")]).unwrap();
        assert!(out.contains("\r\n"), "CRLF lost: {out:?}");
        assert!(
            !out.replace("\r\n", "").contains('\n'),
            "mixed EOL: {out:?}"
        );
        assert!(out.contains("# keep"), "{out:?}");
    }

    /// One malformed provider value must not make the whole scope unreadable;
    /// the well-formed sibling routes still come back.
    #[test]
    fn test_malformed_route_value_is_skipped() {
        let raw = "- name: '@deepseek-ai/dsh-llm-pi-ai'\n  config:\n    providers:\n      broken: https://not-a-mapping\n      good:\n        api: openai-completions\n        baseURL: https://x/v1\n";
        let routes = parse_providers(raw).unwrap();
        assert_eq!(routes.len(), 1, "{routes:?}");
        assert_eq!(routes[0].name, "good");
    }

    /// End-to-end through a real file on disk: add a route, read it back, add a
    /// second, then remove both and confirm the `[]` placeholder returns.
    #[test]
    fn test_file_round_trip() {
        let home = std::env::temp_dir().join(format!("dsh-prov-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("profiles").join("main")).unwrap();
        let path = patch_path(&home, Some("main")).unwrap();

        // DSH ships an empty profile patch as the literal `[]`.
        write_patch(&path, "[]\n").unwrap();

        let raw = read_patch(&path).unwrap();
        let mut routes = parse_providers(&raw).unwrap();
        assert!(routes.is_empty());
        routes.push(route("acme-gateway"));
        write_patch(&path, &render_providers(&raw, &routes).unwrap()).unwrap();

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains(PI_AI_MODULE), "{on_disk}");
        assert!(on_disk.contains("acme-gateway"), "{on_disk}");
        assert!(!on_disk.lines().any(|l| l.trim() == "[]"), "{on_disk}");

        // A second route must coexist in the same managed entry.
        let raw = read_patch(&path).unwrap();
        let mut routes = parse_providers(&raw).unwrap();
        routes.push(route("second"));
        routes.sort_by(|a, b| a.name.cmp(&b.name));
        write_patch(&path, &render_providers(&raw, &routes).unwrap()).unwrap();

        let both = parse_providers(&read_patch(&path).unwrap()).unwrap();
        assert_eq!(
            both.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["acme-gateway", "second"]
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches(PI_AI_MODULE).count(), 1, "{text}");

        // Removing every route restores the placeholder.
        let raw = read_patch(&path).unwrap();
        write_patch(&path, &render_providers(&raw, &[]).unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "[]");

        let _ = std::fs::remove_dir_all(&home);
    }
}
