/// Provider 预设模块 (Issue #76)
///
/// 提供常用 Provider 的预设路由，方便用户快速配置。
///
/// 预设只是编辑器的起点：字段生成后仍可自由修改。`models` 以**对象**形式给出，
/// `api` 使用线上协议名（见 [`crate::provider_config::SUPPORTED_APIS`]）。
use crate::provider_config::{ProviderModel, ProviderRoute};

/// Provider 预设
#[derive(Clone, Debug)]
#[allow(dead_code)] // 预设元数据属于 issue #76 的模块 API；目前只有 id/template 被读取。
pub struct ProviderPreset {
    /// 预设 ID
    pub id: &'static str,
    /// 显示名称
    pub name: &'static str,
    /// 描述
    pub description: &'static str,
    /// 默认路由名称（用户可修改）
    pub default_route_name: &'static str,
    /// 默认配置生成函数
    pub template: fn() -> ProviderRoute,
}

fn model(id: &str) -> ProviderModel {
    ProviderModel {
        id: id.to_string(),
        ..Default::default()
    }
}

/// 获取所有内置预设
pub fn get_all_presets() -> Vec<ProviderPreset> {
    vec![
        PRESET_DEEPSEEK_OFFICIAL,
        PRESET_OPENAI_COMPATIBLE,
        PRESET_ANTHROPIC,
        PRESET_CUSTOM_ENDPOINT,
    ]
}

/// 根据 ID 获取预设
pub fn get_preset_by_id(id: &str) -> Option<ProviderPreset> {
    get_all_presets().into_iter().find(|p| p.id == id)
}

/// 根据预设名获取路由列表（单个预设生成一条路由）。
pub fn get_preset(preset_name: &str) -> Result<Vec<ProviderRoute>, String> {
    let preset =
        get_preset_by_id(preset_name).ok_or_else(|| format!("未找到预设 '{}'", preset_name))?;

    Ok(vec![(preset.template)()])
}

// ---------------------------------------------------------------------------
// 预设定义
// ---------------------------------------------------------------------------

/// DeepSeek 官方
pub const PRESET_DEEPSEEK_OFFICIAL: ProviderPreset = ProviderPreset {
    id: "deepseek-official",
    name: "DeepSeek 官方",
    description: "DeepSeek 官方 API 端点，支持 deepseek-chat 和 deepseek-reasoner 模型",
    default_route_name: "deepseek-official",
    template: || ProviderRoute {
        name: "deepseek-official".to_string(),
        api_key_env: Some("DEEPSEEK_API_KEY".to_string()),
        api: Some("anthropic-messages".to_string()),
        base_url: Some("https://api.deepseek.com/anthropic".to_string()),
        models: vec![model("deepseek-chat"), model("deepseek-reasoner")],
        ..Default::default()
    },
};

/// OpenAI 兼容网关
pub const PRESET_OPENAI_COMPATIBLE: ProviderPreset = ProviderPreset {
    id: "openai-compatible",
    name: "OpenAI 兼容端点",
    description: "OpenAI 官方 API 或兼容 OpenAI 协议的第三方网关",
    default_route_name: "openai-compat",
    template: || ProviderRoute {
        name: "openai-compat".to_string(),
        api_key_env: Some("OPENAI_API_KEY".to_string()),
        api: Some("openai-completions".to_string()),
        base_url: Some("https://api.openai.com/v1".to_string()),
        models: vec![model("gpt-4o"), model("gpt-4o-mini")],
        ..Default::default()
    },
};

/// Anthropic 官方
pub const PRESET_ANTHROPIC: ProviderPreset = ProviderPreset {
    id: "anthropic-official",
    name: "Anthropic 官方",
    description: "Anthropic 官方 API 端点，支持 Claude 系列模型",
    default_route_name: "anthropic-official",
    template: || ProviderRoute {
        name: "anthropic-official".to_string(),
        api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
        api: Some("anthropic-messages".to_string()),
        base_url: Some("https://api.anthropic.com".to_string()),
        models: vec![
            model("claude-sonnet-5-5"),
            model("claude-opus-5-5"),
            model("claude-haiku-4-5"),
        ],
        ..Default::default()
    },
};

/// 自定义端点（空白模板）
pub const PRESET_CUSTOM_ENDPOINT: ProviderPreset = ProviderPreset {
    id: "custom-endpoint",
    name: "自定义端点",
    description: "空白模板，手动填写所有配置字段",
    default_route_name: "custom-gateway",
    template: || ProviderRoute {
        name: "custom-gateway".to_string(),
        api_key_env: Some("CUSTOM_API_KEY".to_string()),
        api: Some("openai-completions".to_string()),
        base_url: Some("https://your-api.example.com/v1".to_string()),
        models: vec![model("your-model-name")],
        ..Default::default()
    },
};

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 生成预设配置并自定义路由名称
#[allow(dead_code)] // 属于 issue #76 的模块 API；当前命令面由 get_preset 覆盖。
pub fn create_from_preset(preset_id: &str, custom_name: Option<&str>) -> Option<ProviderRoute> {
    let preset = get_preset_by_id(preset_id)?;
    let mut route = (preset.template)();

    if let Some(name) = custom_name {
        route.name = name.to_string();
    }

    Some(route)
}

/// 列出所有可用的预设 ID。
pub fn list_presets() -> Vec<String> {
    get_all_presets()
        .into_iter()
        .map(|p| p.id.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_presets_have_unique_ids() {
        let presets = get_all_presets();
        let mut ids = std::collections::HashSet::new();

        for preset in &presets {
            assert!(ids.insert(preset.id), "重复的预设 ID: {}", preset.id);
        }

        assert_eq!(ids.len(), presets.len());
    }

    #[test]
    fn test_get_preset_by_id() {
        assert!(get_preset_by_id("deepseek-official").is_some());
        assert!(get_preset_by_id("openai-compatible").is_some());
        assert!(get_preset_by_id("anthropic-official").is_some());
        assert!(get_preset_by_id("custom-endpoint").is_some());
        assert!(get_preset_by_id("nonexistent").is_none());
    }

    #[test]
    fn test_list_presets_matches_get_all() {
        let listed = list_presets();
        assert_eq!(listed.len(), get_all_presets().len());
        for id in &listed {
            assert!(get_preset_by_id(id).is_some(), "列出的 '{id}' 无对应预设");
        }
    }

    #[test]
    fn test_deepseek_official_preset() {
        let route = (PRESET_DEEPSEEK_OFFICIAL.template)();
        assert_eq!(route.api_key_env.as_deref(), Some("DEEPSEEK_API_KEY"));
        assert_eq!(
            route.base_url.as_deref(),
            Some("https://api.deepseek.com/anthropic")
        );
        assert_eq!(route.api.as_deref(), Some("anthropic-messages"));
        let ids: Vec<&str> = route.models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"deepseek-chat"));
        assert!(ids.contains(&"deepseek-reasoner"));
    }

    #[test]
    fn test_openai_compatible_preset() {
        let route = (PRESET_OPENAI_COMPATIBLE.template)();
        assert_eq!(route.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
        assert_eq!(route.base_url.as_deref(), Some("https://api.openai.com/v1"));
        assert_eq!(route.api.as_deref(), Some("openai-completions"));
        assert!(!route.models.is_empty());
    }

    #[test]
    fn test_create_from_preset_with_custom_name() {
        let route = create_from_preset("deepseek-official", Some("my-deepseek")).unwrap();
        assert_eq!(route.name, "my-deepseek");
        assert_eq!(route.api_key_env.as_deref(), Some("DEEPSEEK_API_KEY"));
    }

    #[test]
    fn test_create_from_preset_default_name() {
        let route = create_from_preset("deepseek-official", None).unwrap();
        assert_eq!(route.name, "deepseek-official");
    }

    #[test]
    fn test_all_presets_generate_valid_routes() {
        for preset in get_all_presets() {
            let route = (preset.template)();
            assert!(!route.name.is_empty(), "预设 {} 的 name 为空", preset.id);
            crate::provider_config::validate_route(&route)
                .unwrap_or_else(|e| panic!("预设 {} 生成的路由无效: {e}", preset.id));
        }
    }
}
