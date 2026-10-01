/// Provider 验证模块 (Issue #76)
///
/// 负责预启动验证 Provider 配置：路由可解析性、凭据存在性、baseURL 格式、模型目录有效性等。
use crate::credential_manager;
use crate::provider_config::{self, ProviderRoute};
use std::collections::BTreeMap;
use std::path::Path;

// ---------------------------------------------------------------------------
// 数据结构
// ---------------------------------------------------------------------------

/// 验证报告
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationReport {
    /// 各路由的验证结果
    pub routes: Vec<RouteValidation>,
    /// 是否存在阻塞性错误
    pub has_blocking_errors: bool,
}

impl ValidationReport {
    /// 计算统计信息
    #[allow(dead_code)] // Part of the issue #76 module API; the UI computes its own tallies.
    pub fn stats(&self) -> ValidationStats {
        let mut stats = ValidationStats::default();
        for route in &self.routes {
            match route.status {
                ValidationStatus::Ok => stats.ok_count += 1,
                ValidationStatus::Warning => stats.warning_count += 1,
                ValidationStatus::Error => stats.error_count += 1,
                ValidationStatus::Unknown => stats.unknown_count += 1,
            }
        }
        stats
    }
}

/// 验证统计
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // Part of the issue #76 module API; returned by ValidationReport::stats.
pub struct ValidationStats {
    pub ok_count: usize,
    pub warning_count: usize,
    pub error_count: usize,
    pub unknown_count: usize,
}

/// 单个路由的验证结果
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteValidation {
    /// 路由名称
    pub name: String,
    /// 验证状态
    pub status: ValidationStatus,
    /// 验证消息列表（问题描述、建议等）
    pub messages: Vec<String>,
}

/// 验证状态
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ValidationStatus {
    /// 确认正常
    Ok,
    /// 警告（不阻塞启动，但需注意）
    Warning,
    /// 错误（可能导致运行时失败）
    Error,
    /// 无法确定（缺少检查条件）
    Unknown,
}

impl ValidationStatus {
    /// 获取状态的显示图标
    #[allow(dead_code)] // Part of the issue #76 module API; the UI renders its own icons/labels.
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Ok => "✓",
            Self::Warning => "⚠",
            Self::Error => "✗",
            Self::Unknown => "❓",
        }
    }

    /// 获取状态的显示名称
    #[allow(dead_code)] // Part of the issue #76 module API; the UI localizes its own labels.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Ok => "正常",
            Self::Warning => "警告",
            Self::Error => "错误",
            Self::Unknown => "未知",
        }
    }
}

// ---------------------------------------------------------------------------
// 核心 API
// ---------------------------------------------------------------------------

/// 验证一组 provider 路由（离线，不发起任何网络请求）。
///
/// # 参数
/// - `routes`: 要验证的路由
/// - `dsh_home`: DSH_HOME 目录路径
/// - `profile_path`: Profile 目录路径（用于检查 profile `.env`）
/// - `env_overrides`: 实例的环境变量覆盖
pub fn validate_routes(
    routes: &[ProviderRoute],
    dsh_home: &Path,
    profile_path: &Path,
    env_overrides: &BTreeMap<String, String>,
) -> ValidationReport {
    let mut validations = Vec::new();
    let mut has_blocking_errors = false;

    for route in routes {
        let validation = validate_single_route(dsh_home, profile_path, env_overrides, route);

        if validation.status == ValidationStatus::Error {
            has_blocking_errors = true;
        }

        validations.push(validation);
    }

    ValidationReport {
        routes: validations,
        has_blocking_errors,
    }
}

/// 验证单个路由
fn validate_single_route(
    dsh_home: &Path,
    profile_path: &Path,
    env_overrides: &BTreeMap<String, String>,
    route: &ProviderRoute,
) -> RouteValidation {
    let mut messages = Vec::new();
    let mut status = ValidationStatus::Ok;

    // 1. 验证路由名称格式
    if let Err(e) = provider_config::validate_route_name(&route.name) {
        messages.push(format!("路由名称格式错误: {}", e));
        status = ValidationStatus::Error;
    }

    // 2. 验证 apiKeyEnv 格式（可省略 = 已配置但无密钥）
    let key = route.api_key_env_str();
    if key.is_empty() {
        messages.push("未指定 apiKeyEnv（该路由将走 provider 原生环境认证）".to_string());
        if status == ValidationStatus::Ok {
            status = ValidationStatus::Warning;
        }
    } else if !is_valid_env_key(key) {
        messages.push(format!("apiKeyEnv '{}' 格式不合法", key));
        status = ValidationStatus::Error;
    } else {
        // 3. 验证凭据存在性
        match credential_manager::get_credential(dsh_home, Some(profile_path), env_overrides, key) {
            Ok(Some(cred)) => {
                messages.push(format!(
                    "凭据已配置（来源: {}）",
                    cred.source.display_name()
                ));
            }
            Ok(None) => {
                messages.push(format!("缺少凭据: {}", key));
                status = ValidationStatus::Error;
            }
            Err(e) => {
                messages.push(format!("凭据检查失败: {}", e));
                status = ValidationStatus::Unknown;
            }
        }
    }

    // 4. 验证协议
    match route.api.as_deref().filter(|s| !s.is_empty()) {
        Some(api) => {
            if let Err(e) = provider_config::validate_api(api) {
                messages.push(format!("api 验证失败: {}", e));
                status = ValidationStatus::Error;
            }
        }
        None => messages.push("未声明 api（沿用已安装目录的协议）".to_string()),
    }

    // 5. 验证 baseURL（可省略 = 沿用目录端点）
    match route.base_url.as_deref().filter(|s| !s.is_empty()) {
        Some(url) => {
            if let Err(e) = provider_config::validate_base_url(url) {
                messages.push(format!("baseURL 验证失败: {}", e));
                if status == ValidationStatus::Ok {
                    status = ValidationStatus::Warning;
                }
            } else if !url.starts_with("https://") {
                messages.push("建议使用 HTTPS 端点以确保安全".to_string());
                if status == ValidationStatus::Ok {
                    status = ValidationStatus::Warning;
                }
            }
            // OpenAI 兼容协议通常带 /v1 路径。
            if route.api.as_deref() == Some("openai-completions") && !url.contains("/v1") {
                messages.push("openai-completions 协议通常需要 /v1 路径".to_string());
                if status == ValidationStatus::Ok {
                    status = ValidationStatus::Warning;
                }
            }
        }
        None => messages.push("未声明 baseURL（沿用已安装目录的端点）".to_string()),
    }

    // 6. 验证模型目录（可省略 = 沿用已安装目录）
    if route.models.is_empty() {
        messages.push("未声明 models（沿用已安装目录的模型）".to_string());
    } else if let Err(e) = provider_config::validate_models(&route.models) {
        messages.push(format!("模型列表验证失败: {}", e));
        status = ValidationStatus::Error;
    } else {
        messages.push(format!("包含 {} 个模型", route.models.len()));
    }

    // 如果没有任何问题，添加成功消息
    if status == ValidationStatus::Ok && messages.len() <= 2 {
        messages.insert(0, "配置正确，可以正常使用".to_string());
    }

    RouteValidation {
        name: route.name.clone(),
        status,
        messages,
    }
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 验证环境变量键名格式
fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();

    // 第一个字符必须是字母或下划线
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }

    // 后续字符可以是字母、数字或下划线
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_config::{ProviderModel, ProviderRoute};
    use tempfile::TempDir;

    #[test]
    fn test_is_valid_env_key() {
        assert!(is_valid_env_key("DEEPSEEK_API_KEY"));
        assert!(is_valid_env_key("_PRIVATE_KEY"));
        assert!(is_valid_env_key("VAR123"));

        assert!(!is_valid_env_key("123VAR"));
        assert!(!is_valid_env_key("VAR-NAME"));
        assert!(!is_valid_env_key(""));
    }

    #[test]
    fn test_validation_status_display() {
        assert_eq!(ValidationStatus::Ok.icon(), "✓");
        assert_eq!(ValidationStatus::Warning.icon(), "⚠");
        assert_eq!(ValidationStatus::Error.icon(), "✗");
        assert_eq!(ValidationStatus::Unknown.icon(), "❓");

        assert_eq!(ValidationStatus::Ok.display_name(), "正常");
        assert_eq!(ValidationStatus::Warning.display_name(), "警告");
        assert_eq!(ValidationStatus::Error.display_name(), "错误");
        assert_eq!(ValidationStatus::Unknown.display_name(), "未知");
    }

    #[test]
    fn test_validate_single_route_with_valid_config() {
        let temp = TempDir::new().unwrap();
        let dsh_home = temp.path();
        let profile_path = temp.path().join("profile");
        std::fs::create_dir_all(&profile_path).unwrap();

        // 创建凭据
        std::fs::write(
            dsh_home.join(".credentials.yaml"),
            "refs:\n  TEST_API_KEY: sk-test123456789\n",
        )
        .unwrap();

        let route = ProviderRoute {
            name: "test-gateway".to_string(),
            api_key_env: Some("TEST_API_KEY".to_string()),
            base_url: Some("https://api.test.com/v1".to_string()),
            api: Some("openai-completions".to_string()),
            models: vec![ProviderModel {
                id: "model1".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let env_overrides = BTreeMap::new();
        let validation = validate_single_route(dsh_home, &profile_path, &env_overrides, &route);

        assert_eq!(validation.status, ValidationStatus::Ok);
        assert!(validation.messages.iter().any(|m| m.contains("凭据已配置")));
    }

    #[test]
    fn test_validate_single_route_missing_credential() {
        let temp = TempDir::new().unwrap();
        let dsh_home = temp.path();
        let profile_path = temp.path().join("profile");
        std::fs::create_dir_all(&profile_path).unwrap();

        let route = ProviderRoute {
            name: "test-gateway".to_string(),
            api_key_env: Some("MISSING_KEY".to_string()),
            base_url: Some("https://api.test.com/v1".to_string()),
            api: Some("openai-completions".to_string()),
            models: vec![ProviderModel {
                id: "model1".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let env_overrides = BTreeMap::new();
        let validation = validate_single_route(dsh_home, &profile_path, &env_overrides, &route);

        assert_eq!(validation.status, ValidationStatus::Error);
        assert!(validation.messages.iter().any(|m| m.contains("缺少凭据")));
    }

    #[test]
    fn test_validate_single_route_invalid_base_url() {
        let temp = TempDir::new().unwrap();
        let dsh_home = temp.path();
        let profile_path = temp.path().join("profile");
        std::fs::create_dir_all(&profile_path).unwrap();

        // 创建凭据
        std::fs::write(
            dsh_home.join(".credentials.yaml"),
            "refs:\n  TEST_API_KEY: sk-test123456789\n",
        )
        .unwrap();

        let route = ProviderRoute {
            name: "test-gateway".to_string(),
            api_key_env: Some("TEST_API_KEY".to_string()),
            base_url: Some("invalid-url".to_string()), // 无效的 URL
            api: Some("openai-completions".to_string()),
            models: vec![ProviderModel {
                id: "model1".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let env_overrides = BTreeMap::new();
        let validation = validate_single_route(dsh_home, &profile_path, &env_overrides, &route);

        // 应该至少是警告状态
        assert!(
            validation.status == ValidationStatus::Warning
                || validation.status == ValidationStatus::Error
        );
        assert!(validation
            .messages
            .iter()
            .any(|m| m.contains("baseURL") || m.contains("URL")));
    }

    #[test]
    fn test_validate_single_route_empty_models() {
        let temp = TempDir::new().unwrap();
        let dsh_home = temp.path();
        let profile_path = temp.path().join("profile");
        std::fs::create_dir_all(&profile_path).unwrap();

        // 创建凭据
        std::fs::write(
            dsh_home.join(".credentials.yaml"),
            "refs:\n  TEST_API_KEY: sk-test123456789\n",
        )
        .unwrap();

        let route = ProviderRoute {
            name: "test-gateway".to_string(),
            api_key_env: Some("TEST_API_KEY".to_string()),
            base_url: Some("https://api.test.com/v1".to_string()),
            api: Some("openai-completions".to_string()),
            models: vec![], // 空模型列表：沿用已安装目录，不是错误
            ..Default::default()
        };

        let env_overrides = BTreeMap::new();
        let validation = validate_single_route(dsh_home, &profile_path, &env_overrides, &route);

        assert_eq!(validation.status, ValidationStatus::Ok);
        assert!(validation
            .messages
            .iter()
            .any(|m| m.contains("未声明 models")));
    }

    #[test]
    fn test_validation_report_stats() {
        let report = ValidationReport {
            routes: vec![
                RouteValidation {
                    name: "route1".to_string(),
                    status: ValidationStatus::Ok,
                    messages: vec![],
                },
                RouteValidation {
                    name: "route2".to_string(),
                    status: ValidationStatus::Warning,
                    messages: vec![],
                },
                RouteValidation {
                    name: "route3".to_string(),
                    status: ValidationStatus::Error,
                    messages: vec![],
                },
            ],
            has_blocking_errors: true,
        };

        let stats = report.stats();
        assert_eq!(stats.ok_count, 1);
        assert_eq!(stats.warning_count, 1);
        assert_eq!(stats.error_count, 1);
        assert_eq!(stats.unknown_count, 0);
    }
}
