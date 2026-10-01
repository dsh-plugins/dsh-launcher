/// Provider 配置核心模块 (Issue #76)
///
/// 负责 provider 路由的数据结构、验证与序列化。
///
/// 存储位置：配置文件是 **profile 的 `cordis.patch.yml`**（或全局的
/// `<DSH_HOME>/cordis.patch.yml`），形如一个 loader patch 条目：
///
/// ```yaml
/// - name: '@deepseek-ai/dsh-llm-pi-ai'
///   config:
///     providers:
///       deepseek-official:
///         apiKeyEnv: DEEPSEEK_API_KEY
///         baseURL: https://api.deepseek.com/anthropic
///         api: anthropic-messages
///         models:
///           - id: deepseek-chat
///             contextWindow: 1000000
/// ```
///
/// 注意：`config.providers` 是以**路由名称为键的字典**，而非列表；`api` 是
/// **线上协议**（`anthropic-messages` / `openai-completions` / `openai-responses`），
/// 不是 `anthropic`/`openai`；`models` 的每一项是**对象**而非字符串。
///
/// 本模块只负责数据与验证；文件的读写拼接见 [`crate::provider_patch`]。
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// 数据结构
// ---------------------------------------------------------------------------

/// 支持的线上协议（对应 pi-ai 的 `api` 字段）。
///
/// 仅覆盖「一个 key + 一个端点 + 一组模型」可以完整描述的路由；Bedrock /
/// Vertex / Azure / Codex 等需要专有认证流程的路由不在其中。
pub const SUPPORTED_APIS: [&str; 3] = [
    "anthropic-messages",
    "openai-completions",
    "openai-responses",
];

/// 单个模型条目。`id` 必填，其余字段省略时回落到已安装目录的默认值。
///
/// 编辑器只暴露 `id` / `name` / `context_window` / `reasoning_efforts`；
/// 其余键通过 `extra` 原样保留，避免改写用户手写的配置。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// 每个可选思考等级 -> 线上拼写；`false` 表示非推理模型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_efforts: Option<serde_json::Value>,
    /// 未识别的键，保存时原样写回。
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// 一条 provider 路由（`config.providers` 中的一个条目）。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRoute {
    /// 路由名称：`config.providers` 字典的键（解析时由键回填）。
    #[serde(default)]
    pub name: String,
    /// 凭据引用（环境变量名）。省略表示「已配置但无密钥」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// 显示名称。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// 线上协议。仅手写路由需要。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    /// 端点。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// 模型目录：整体替换该路由的已安装目录。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ProviderModel>,
    /// 未识别的键（`compat` / `retryPolicy` / `defaultContextWindow` ...），
    /// 保存时原样写回。
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl ProviderRoute {
    pub fn api_key_env_str(&self) -> &str {
        self.api_key_env.as_deref().unwrap_or("")
    }
}

// ---------------------------------------------------------------------------
// 验证
// ---------------------------------------------------------------------------

/// 验证路由名称（只允许字母、数字、连字符、下划线）
pub fn validate_route_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("路由名称不能为空".to_string());
    }
    if name.len() > 64 {
        return Err("路由名称过长（最多 64 字符）".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err("路由名称只能包含字母、数字、连字符和下划线".to_string());
    }
    Ok(())
}

/// 验证 base URL（必须是有效的 HTTP(S) URL）
pub fn validate_base_url(url: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("Base URL 不能为空".to_string());
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("Base URL 必须以 http:// 或 https:// 开头".to_string());
    }
    if url.contains(char::is_whitespace) {
        return Err("Base URL 不能包含空白字符".to_string());
    }
    Ok(())
}

/// 验证线上协议是否在支持集合内。
pub fn validate_api(api: &str) -> Result<(), String> {
    if SUPPORTED_APIS.contains(&api) {
        Ok(())
    } else {
        Err(format!(
            "不支持的 API 协议 '{}'（可选：{}）",
            api,
            SUPPORTED_APIS.join(", ")
        ))
    }
}

/// 验证模型列表（至少一个模型，且每个 id 非空）
pub fn validate_models(models: &[ProviderModel]) -> Result<(), String> {
    if models.is_empty() {
        return Err("至少需要配置一个模型".to_string());
    }
    for model in models {
        if model.id.trim().is_empty() {
            return Err("模型 id 不能为空".to_string());
        }
    }
    Ok(())
}

/// 验证一条路由。手写路由（目录未描述的）必须有 `api`、`baseURL` 和非空 `models`。
pub fn validate_route(route: &ProviderRoute) -> Result<(), String> {
    validate_route_name(&route.name)?;
    if let Some(api) = route.api.as_deref().filter(|s| !s.is_empty()) {
        validate_api(api)?;
    }
    if let Some(url) = route.base_url.as_deref().filter(|s| !s.is_empty()) {
        validate_base_url(url)?;
    }
    // 目录已描述的路由可以省略 models（沿用已安装目录）；一旦声明就必须合法。
    if !route.models.is_empty() {
        validate_models(&route.models)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 模板导出 / 导入（Issue #76 §4.2，整合包支持）
// ---------------------------------------------------------------------------

/// 从一组路由生成可随整合包分发的模板，**屏蔽所有凭据**：
/// `apiKeyEnv` 替换为占位符，未识别的键（可能内含密钥的 `headers` 等）一律丢弃。
pub fn export_provider_templates(routes: &[ProviderRoute]) -> Result<String, String> {
    let mut masked: Vec<ProviderRoute> = Vec::with_capacity(routes.len());
    for route in routes {
        let mut copy = route.clone();
        if copy.api_key_env.is_some() {
            copy.api_key_env = Some("YOUR_API_KEY_ENV_VAR".to_string());
        }
        copy.extra.clear();
        for model in &mut copy.models {
            model.extra.clear();
        }
        masked.push(copy);
    }
    serde_yaml::to_string(&masked).map_err(|e| format!("序列化 provider 模板失败: {e}"))
}

/// 解析模板 YAML 为路由列表。
pub fn parse_templates(template_yaml: &str) -> Result<Vec<ProviderRoute>, String> {
    serde_yaml::from_str(template_yaml).map_err(|e| format!("解析 provider 模板失败: {e}"))
}

/// 合并模板路由到现有列表（追加模式，按名称去重）。返回新增数量。
///
/// 已存在的同名路由**保留不动**，以免导入一个包就悄悄改写用户手改过的配置。
pub fn merge_templates(existing: &mut Vec<ProviderRoute>, incoming: Vec<ProviderRoute>) -> usize {
    let mut added = 0usize;
    for route in incoming {
        if existing.iter().any(|r| r.name == route.name) {
            continue;
        }
        existing.push(route);
        added += 1;
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str) -> ProviderModel {
        ProviderModel {
            id: id.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_validate_route_name() {
        assert!(validate_route_name("deepseek-official").is_ok());
        assert!(validate_route_name("openai_v1").is_ok());
        assert!(validate_route_name("").is_err());
        assert!(validate_route_name("invalid name").is_err());
    }

    #[test]
    fn test_validate_base_url() {
        assert!(validate_base_url("https://api.deepseek.com").is_ok());
        assert!(validate_base_url("http://localhost:8080/v1").is_ok());
        assert!(validate_base_url("invalid-url").is_err());
        assert!(validate_base_url("").is_err());
        assert!(validate_base_url("https://a b.example").is_err());
    }

    #[test]
    fn test_validate_api() {
        assert!(validate_api("anthropic-messages").is_ok());
        assert!(validate_api("openai-completions").is_ok());
        assert!(validate_api("anthropic").is_err());
        assert!(validate_api("").is_err());
    }

    #[test]
    fn test_validate_models() {
        assert!(validate_models(&[model("deepseek-chat")]).is_ok());
        assert!(validate_models(&[]).is_err());
        assert!(validate_models(&[model("  ")]).is_err());
    }

    #[test]
    fn test_route_round_trip_preserves_unknown_keys() {
        let yaml = r#"
name: acme-gateway
displayName: Acme Gateway
apiKeyEnv: ACME_GATEWAY_API_KEY
api: openai-completions
baseURL: https://gateway.acme.example/v1
compat:
  thinkingFormat: deepseek
retryPolicy:
  mode: normal
  maxRetries: 3
models:
  - id: acme-think
    name: Acme Think
    contextWindow: 262144
    reasoningEfforts:
      off:
      high: high
"#;
        let route: ProviderRoute = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(route.name, "acme-gateway");
        assert_eq!(route.api.as_deref(), Some("openai-completions"));
        assert_eq!(route.models[0].context_window, Some(262144));
        // Unknown top-level keys survive in `extra`.
        assert!(route.extra.contains_key("compat"));
        assert!(route.extra.contains_key("retryPolicy"));

        // Re-serializing keeps them.
        let out = serde_yaml::to_string(&route).unwrap();
        assert!(out.contains("thinkingFormat"), "{out}");
        assert!(out.contains("maxRetries"), "{out}");
    }

    #[test]
    fn test_export_provider_templates_masks_secrets() {
        let routes = vec![ProviderRoute {
            name: "deepseek-official".to_string(),
            api_key_env: Some("DEEPSEEK_API_KEY".to_string()),
            base_url: Some("https://api.deepseek.com/anthropic".to_string()),
            api: Some("anthropic-messages".to_string()),
            models: vec![model("deepseek-chat")],
            extra: BTreeMap::from([(
                "headers".to_string(),
                serde_json::json!({ "X-Secret": "secret-value" }),
            )]),
            ..Default::default()
        }];

        let out = export_provider_templates(&routes).unwrap();
        assert!(out.contains("YOUR_API_KEY_ENV_VAR"), "{out}");
        assert!(!out.contains("DEEPSEEK_API_KEY"), "{out}");
        assert!(!out.contains("secret-value"), "{out}");
        assert!(out.contains("deepseek-official"), "{out}");
    }

    #[test]
    fn test_merge_templates_skips_existing() {
        let mut existing = vec![ProviderRoute {
            name: "keep-me".to_string(),
            api_key_env: Some("K".to_string()),
            ..Default::default()
        }];
        let incoming = vec![
            ProviderRoute {
                name: "keep-me".to_string(),
                api_key_env: Some("YOUR_API_KEY_ENV_VAR".to_string()),
                ..Default::default()
            },
            ProviderRoute {
                name: "added".to_string(),
                ..Default::default()
            },
        ];
        let added = merge_templates(&mut existing, incoming);
        assert_eq!(added, 1);
        assert_eq!(existing.len(), 2);
        // The pre-existing route is untouched.
        assert_eq!(existing[0].api_key_env.as_deref(), Some("K"));
    }
}
