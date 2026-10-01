/// 凭据管理模块 (Issue #76)
///
/// 负责按优先级解析凭据、读写 <DSH_HOME>/.credentials.yaml、掩码敏感信息等。
///
/// 凭据解析优先级（从高到低）：
/// 1. 实例 env_overrides（instance_config.json 中的环境变量覆盖）
/// 2. <DSH_HOME>/.credentials.yaml 的 refs 部分
/// 3. Profile 目录的 .env 文件
/// 4. $DSH_HOME/.env 文件
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

// ---------------------------------------------------------------------------
// 数据结构
// ---------------------------------------------------------------------------

/// 凭据来源信息
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CredentialSource {
    /// 凭据键名（如 DEEPSEEK_API_KEY）
    pub key: String,
    /// 掩码后的值（如 sk-***abc1）
    pub value: String,
    /// 来源层级
    pub source: CredentialLayer,
    /// 是否被更高优先级的层覆盖
    pub is_overridden: bool,
}

/// 凭据来源层级（按优先级排序）
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialLayer {
    /// 实例环境变量覆盖（最高优先级）
    EnvOverride,
    /// <DSH_HOME>/.credentials.yaml
    CredentialsYaml,
    /// Profile 目录的 .env
    ProjectDotEnv,
    /// $DSH_HOME/.env（最低优先级）
    HomeDotEnv,
}

impl CredentialLayer {
    /// 获取层级的显示名称
    #[allow(dead_code)] // Part of the issue #76 module API; the UI localizes its own labels.
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::EnvOverride => "实例环境变量",
            Self::CredentialsYaml => ".credentials.yaml",
            Self::ProjectDotEnv => "Profile .env",
            Self::HomeDotEnv => "HOME .env",
        }
    }
}

/// .credentials.yaml 文件结构
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct CredentialsYaml {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    refs: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// 核心 API
// ---------------------------------------------------------------------------

/// 按优先级获取凭据
///
/// # 参数
/// - `dsh_home`: DSH_HOME 目录路径
/// - `profile_path`: Profile 目录路径（可选，用于检查 Profile .env）
/// - `env_overrides`: 实例的环境变量覆盖
/// - `key`: 凭据键名
pub fn get_credential(
    dsh_home: &Path,
    profile_path: Option<&Path>,
    env_overrides: &BTreeMap<String, String>,
    key: &str,
) -> Result<Option<CredentialSource>, String> {
    // 1. 检查 env_overrides（最高优先级）
    if let Some(value) = env_overrides.get(key) {
        return Ok(Some(CredentialSource {
            key: key.to_string(),
            value: mask_api_key(value),
            source: CredentialLayer::EnvOverride,
            is_overridden: false,
        }));
    }

    // 2. 检查 .credentials.yaml
    let creds_path = dsh_home.join(".credentials.yaml");
    if creds_path.exists() {
        if let Ok(content) = fs::read_to_string(&creds_path) {
            if let Ok(creds) = serde_yaml::from_str::<CredentialsYaml>(&content) {
                if let Some(value) = creds.refs.get(key) {
                    return Ok(Some(CredentialSource {
                        key: key.to_string(),
                        value: mask_api_key(value),
                        source: CredentialLayer::CredentialsYaml,
                        is_overridden: false,
                    }));
                }
            }
        }
    }

    // 3. 检查 Profile .env
    if let Some(profile) = profile_path {
        let profile_env = profile.join(".env");
        if let Ok(value) = read_dotenv_key(&profile_env, key) {
            return Ok(Some(CredentialSource {
                key: key.to_string(),
                value: mask_api_key(&value),
                source: CredentialLayer::ProjectDotEnv,
                is_overridden: false,
            }));
        }
    }

    // 4. 检查 $DSH_HOME/.env
    let home_env = dsh_home.join(".env");
    if let Ok(value) = read_dotenv_key(&home_env, key) {
        return Ok(Some(CredentialSource {
            key: key.to_string(),
            value: mask_api_key(&value),
            source: CredentialLayer::HomeDotEnv,
            is_overridden: false,
        }));
    }

    // 未找到
    Ok(None)
}

/// 列出所有层级中的凭据（用于显示覆盖状态）
///
/// 返回所有找到的凭据源，包括被覆盖的低优先级源
#[allow(dead_code)] // Part of the issue #76 module API; get_credential covers the current command surface.
pub fn list_all_credential_sources(
    dsh_home: &Path,
    profile_path: Option<&Path>,
    env_overrides: &BTreeMap<String, String>,
    key: &str,
) -> Result<Vec<CredentialSource>, String> {
    let mut sources = Vec::new();
    let mut found_at: Option<CredentialLayer> = None;

    // 按优先级顺序检查各层

    // 1. env_overrides
    if let Some(value) = env_overrides.get(key) {
        sources.push(CredentialSource {
            key: key.to_string(),
            value: mask_api_key(value),
            source: CredentialLayer::EnvOverride,
            is_overridden: false,
        });
        found_at = Some(CredentialLayer::EnvOverride);
    }

    // 2. .credentials.yaml
    let creds_path = dsh_home.join(".credentials.yaml");
    if creds_path.exists() {
        if let Ok(content) = fs::read_to_string(&creds_path) {
            if let Ok(creds) = serde_yaml::from_str::<CredentialsYaml>(&content) {
                if let Some(value) = creds.refs.get(key) {
                    let is_overridden =
                        found_at.is_some_and(|l| l < CredentialLayer::CredentialsYaml);
                    sources.push(CredentialSource {
                        key: key.to_string(),
                        value: mask_api_key(value),
                        source: CredentialLayer::CredentialsYaml,
                        is_overridden,
                    });
                    if found_at.is_none() {
                        found_at = Some(CredentialLayer::CredentialsYaml);
                    }
                }
            }
        }
    }

    // 3. Profile .env
    if let Some(profile) = profile_path {
        let profile_env = profile.join(".env");
        if let Ok(value) = read_dotenv_key(&profile_env, key) {
            let is_overridden = found_at.is_some_and(|l| l < CredentialLayer::ProjectDotEnv);
            sources.push(CredentialSource {
                key: key.to_string(),
                value: mask_api_key(&value),
                source: CredentialLayer::ProjectDotEnv,
                is_overridden,
            });
            if found_at.is_none() {
                found_at = Some(CredentialLayer::ProjectDotEnv);
            }
        }
    }

    // 4. HOME .env
    let home_env = dsh_home.join(".env");
    if let Ok(value) = read_dotenv_key(&home_env, key) {
        let is_overridden = found_at.is_some_and(|l| l < CredentialLayer::HomeDotEnv);
        sources.push(CredentialSource {
            key: key.to_string(),
            value: mask_api_key(&value),
            source: CredentialLayer::HomeDotEnv,
            is_overridden,
        });
    }

    Ok(sources)
}

/// 设置凭据到 .credentials.yaml
///
/// 仅写入 <DSH_HOME>/.credentials.yaml 的 refs 部分。
/// 如果该凭据已由 env_overrides 提供，返回错误。
pub fn set_credential(
    dsh_home: &Path,
    env_overrides: &BTreeMap<String, String>,
    key: &str,
    value: &str,
) -> Result<(), String> {
    // 检查是否被 env_overrides 覆盖
    if env_overrides.contains_key(key) {
        return Err(format!(
            "凭据 {} 已由实例环境变量提供，无法修改。请在实例设置中移除环境变量覆盖后重试。",
            key
        ));
    }

    // 验证键名格式
    if !is_env_key_valid(key) {
        return Err(format!("无效的凭据键名: {}", key));
    }

    let creds_path = dsh_home.join(".credentials.yaml");

    // 读取现有内容
    let mut creds = if creds_path.exists() {
        let content = fs::read_to_string(&creds_path)
            .map_err(|e| format!("读取 .credentials.yaml 失败: {}", e))?;
        serde_yaml::from_str::<CredentialsYaml>(&content).unwrap_or_default()
    } else {
        CredentialsYaml::default()
    };

    // 更新凭据
    creds.refs.insert(key.to_string(), value.to_string());

    // 写入文件（原子写入）
    write_credentials_yaml(&creds_path, &creds)?;

    Ok(())
}

/// 从 .credentials.yaml 删除凭据
pub fn delete_credential(dsh_home: &Path, key: &str) -> Result<(), String> {
    let creds_path = dsh_home.join(".credentials.yaml");

    if !creds_path.exists() {
        return Ok(()); // 文件不存在，视为已删除
    }

    // 读取现有内容
    let content = fs::read_to_string(&creds_path)
        .map_err(|e| format!("读取 .credentials.yaml 失败: {}", e))?;
    let mut creds = serde_yaml::from_str::<CredentialsYaml>(&content).unwrap_or_default();

    // 删除凭据
    if creds.refs.remove(key).is_none() {
        return Ok(()); // 凭据不存在，视为已删除
    }

    // 写入文件
    write_credentials_yaml(&creds_path, &creds)?;

    Ok(())
}

/// 掩码 API Key
///
/// 保留前缀（如 sk-）+ 最后 4 个字符，中间替换为 ***
///
/// 示例：
/// - `sk-abc123def456` -> `sk-***f456`
/// - `short` -> `s***t`
pub fn mask_api_key(value: &str) -> String {
    if value.is_empty() {
        return String::from("(空)");
    }

    // 检查是否有常见前缀
    let prefix_len = if value.starts_with("sk-") || value.starts_with("sk_") {
        3
    } else {
        1 // 保留第一个字符
    };

    let suffix_len = 4;
    let total_len = value.len();

    if total_len <= prefix_len + suffix_len {
        // 太短，只显示首尾各1个字符
        if total_len == 1 {
            return format!("{}***", &value[..1]);
        }
        return format!("{}***{}", &value[..1], &value[total_len - 1..]);
    }

    format!(
        "{}***{}",
        &value[..prefix_len],
        &value[total_len - suffix_len..]
    )
}

// ---------------------------------------------------------------------------
// 内部辅助函数
// ---------------------------------------------------------------------------

/// 从 .env 文件读取指定键的值
fn read_dotenv_key(path: &Path, key: &str) -> Result<String, String> {
    if !path.exists() {
        return Err("文件不存在".to_string());
    }

    let content = fs::read_to_string(path).map_err(|e| format!("读取文件失败: {}", e))?;

    for line in content.lines() {
        let line = line.trim();

        // 跳过注释和空行
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // 解析 KEY=VALUE 格式
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let v = v.trim();

            if k == key {
                // 处理引号
                let v = v.trim_matches('"').trim_matches('\'');
                return Ok(v.to_string());
            }
        }
    }

    Err(format!("未找到键: {}", key))
}

/// 验证环境变量键名是否合法
///
/// 规则：
/// - 第一个字符必须是字母或下划线
/// - 后续字符可以是字母、数字或下划线
fn is_env_key_valid(key: &str) -> bool {
    let mut chars = key.chars();

    // 检查第一个字符
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }

    // 检查后续字符
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 原子写入 .credentials.yaml
fn write_credentials_yaml(path: &Path, creds: &CredentialsYaml) -> Result<(), String> {
    // 创建父目录
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {}", e))?;
    }

    // 序列化
    let content = serde_yaml::to_string(creds).map_err(|e| format!("序列化失败: {}", e))?;

    // 原子写入（临时文件 + 重命名）
    let tmp_path = path.with_extension("yaml.tmp");
    fs::write(&tmp_path, content).map_err(|e| format!("写入临时文件失败: {}", e))?;
    fs::rename(&tmp_path, path).map_err(|e| format!("保存文件失败: {}", e))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_mask_api_key() {
        assert_eq!(mask_api_key("sk-abc123def456"), "sk-***f456");
        assert_eq!(mask_api_key("sk_test1234567890"), "sk_***7890");
        assert_eq!(mask_api_key("short"), "s***t");
        assert_eq!(mask_api_key("ab"), "a***b");
        assert_eq!(mask_api_key("a"), "a***");
        assert_eq!(mask_api_key(""), "(空)");
    }

    #[test]
    fn test_is_env_key_valid() {
        assert!(is_env_key_valid("DEEPSEEK_API_KEY"));
        assert!(is_env_key_valid("_PRIVATE_KEY"));
        assert!(is_env_key_valid("VAR123"));

        assert!(!is_env_key_valid("123VAR")); // 数字开头
        assert!(!is_env_key_valid("VAR-NAME")); // 包含短横线
        assert!(!is_env_key_valid("")); // 空字符串
    }

    #[test]
    fn test_credential_priority() {
        let temp = TempDir::new().unwrap();
        let dsh_home = temp.path();

        // 创建 .credentials.yaml
        let creds_path = dsh_home.join(".credentials.yaml");
        fs::write(&creds_path, "refs:\n  TEST_KEY: value-from-yaml\n").unwrap();

        // 创建 HOME .env
        let home_env = dsh_home.join(".env");
        fs::write(&home_env, "TEST_KEY=value-from-home-env\n").unwrap();

        // 创建 env_overrides
        let mut env_overrides = BTreeMap::new();
        env_overrides.insert("TEST_KEY".to_string(), "value-from-override".to_string());

        // 测试优先级：env_overrides 应该胜出
        let result = get_credential(dsh_home, None, &env_overrides, "TEST_KEY")
            .unwrap()
            .unwrap();
        assert_eq!(result.source, CredentialLayer::EnvOverride);
        assert!(result.value.contains("value-from-override") || result.value.contains("***"));

        // 移除 env_overrides，.credentials.yaml 应该胜出
        let result = get_credential(dsh_home, None, &BTreeMap::new(), "TEST_KEY")
            .unwrap()
            .unwrap();
        assert_eq!(result.source, CredentialLayer::CredentialsYaml);
    }

    #[test]
    fn test_set_and_delete_credential() {
        let temp = TempDir::new().unwrap();
        let dsh_home = temp.path();
        let env_overrides = BTreeMap::new();

        // 设置凭据
        set_credential(dsh_home, &env_overrides, "NEW_KEY", "test-value-123").unwrap();

        // 验证写入
        let result = get_credential(dsh_home, None, &env_overrides, "NEW_KEY")
            .unwrap()
            .unwrap();
        assert_eq!(result.source, CredentialLayer::CredentialsYaml);

        // 删除凭据
        delete_credential(dsh_home, "NEW_KEY").unwrap();

        // 验证删除
        let result = get_credential(dsh_home, None, &env_overrides, "NEW_KEY").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_reject_override_protected_credential() {
        let temp = TempDir::new().unwrap();
        let dsh_home = temp.path();
        let mut env_overrides = BTreeMap::new();
        env_overrides.insert("PROTECTED_KEY".to_string(), "override-value".to_string());

        // 尝试设置被保护的凭据，应该失败
        let result = set_credential(dsh_home, &env_overrides, "PROTECTED_KEY", "new-value");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("实例环境变量"));
    }
}
