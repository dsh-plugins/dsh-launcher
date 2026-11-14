use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::plugins::{MarketDescription, MarketPlugin, MarketPluginDescription, MarketPluginUrls};

/// 官方目录将来源绑定到精确版本，安装时不能根据展示仓库重新推断来源。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum InstallSource {
    Npm {
        registry: Option<String>,
    },
    Git {
        url: String,
        commit: String,
        path: Option<String>,
    },
}

fn web_url(value: &str, https_only: bool) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(value).map_err(|e| format!("无效来源地址: {e}"))?;
    if !(url.scheme() == "https" || (!https_only && url.scheme() == "http"))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("来源地址必须使用 HTTP(S)，且不得包含凭据、查询或片段".into());
    }
    Ok(url)
}

impl InstallSource {
    /// 将已校验的精确来源转换为 pnpm 安装参数。
    pub fn install_spec(&self, name: &str, version: &str) -> Result<String, String> {
        let name_pattern =
            regex::Regex::new(r"^(?:@[a-z0-9][a-z0-9._~-]*/)?[a-z0-9][a-z0-9._~-]*$")
                .map_err(|e| e.to_string())?;
        if name.len() > 214 || !name_pattern.is_match(name) {
            return Err(format!("无效包名 {name}"));
        }
        semver::Version::parse(version).map_err(|e| format!("无效精确版本 {version}: {e}"))?;
        match self {
            Self::Npm { registry } => {
                if let Some(registry) = registry {
                    web_url(registry, false)?;
                }
                Ok(format!("{name}@{version}"))
            }
            Self::Git { url, commit, path } => {
                let parsed = web_url(url, true)?;
                if parsed.path().trim_matches('/').split('/').count() < 2 {
                    return Err("Git 来源必须指向仓库".into());
                }
                if commit.len() != 40
                    || !commit
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                {
                    return Err("Git 来源必须使用 40 位小写提交哈希".into());
                }
                let mut spec = format!("git+{}#{commit}", url.trim_end_matches('/'));
                if let Some(path) = path {
                    if path.split('/').any(|part| {
                        part.is_empty()
                            || part == "."
                            || part == ".."
                            || !part
                                .bytes()
                                .all(|c| c.is_ascii_alphanumeric() || b"._@+-".contains(&c))
                    }) {
                        return Err("Git 包路径必须是仓库内的相对目录".into());
                    }
                    spec.push_str(&format!("&path:/{path}"));
                }
                Ok(spec)
            }
        }
    }

    /// npm 指定的注册表优先于启动器默认设置。
    pub fn registry(&self) -> Option<&str> {
        match self {
            Self::Npm { registry } => registry.as_deref(),
            Self::Git { .. } => None,
        }
    }
}

/// 本地化文本保留地区键，供界面按当前语言选择。插件名称与官方索引标题共用
/// 该类型：字符串表示单语言文本，语言映射表示多语言名称。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum LocalizedText {
    Text(String),
    Languages(BTreeMap<String, String>),
}

impl Default for LocalizedText {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

impl From<String> for LocalizedText {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl LocalizedText {
    /// 语言键统一小写并丢弃空白值，市场站点生成的名称因此可以混用
    /// `en-US` 与 `en-us`；映射键去重后保留首个取值。
    pub fn normalized(self) -> Self {
        match self {
            Self::Text(text) => Self::Text(text.trim().to_string()),
            Self::Languages(texts) => {
                let mut out = BTreeMap::new();
                for (language, text) in texts {
                    let text = text.trim().to_string();
                    if !text.is_empty() {
                        out.entry(language.trim().to_lowercase()).or_insert(text);
                    }
                }
                if out.is_empty() {
                    Self::Text(String::new())
                } else {
                    Self::Languages(out)
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Text(text) => text.trim().is_empty(),
            Self::Languages(texts) => texts.values().all(|text| text.trim().is_empty()),
        }
    }

    /// 全部语言取值，供名称搜索覆盖当前界面语言以外的文本。
    pub fn texts(&self) -> Vec<&str> {
        match self {
            Self::Text(text) => vec![text.as_str()],
            Self::Languages(texts) => texts.values().map(String::as_str).collect(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        let valid = match self {
            Self::Text(text) => !text.trim().is_empty(),
            Self::Languages(texts) => {
                let language = regex::Regex::new(r"^[a-z]{2,8}(?:-[a-z0-9]{1,8})*$")
                    .map_err(|e| e.to_string())?;
                texts.contains_key("en")
                    && texts
                        .iter()
                        .all(|(key, text)| language.is_match(key) && !text.trim().is_empty())
            }
        };
        valid
            .then_some(())
            .ok_or_else(|| "本地化文本必须非空，语言对象必须包含 en".into())
    }

    fn description(self) -> MarketDescription {
        match self {
            Self::Text(text) => MarketDescription::Plain(text),
            Self::Languages(texts) => MarketDescription::Localized(
                texts
                    .into_iter()
                    .map(|(language, content)| MarketPluginDescription { language, content })
                    .collect(),
            ),
        }
    }
}

/// 每个官方版本保留自己的兼容范围和来源。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexVersion {
    pub version: String,
    pub publish_timestamp: u64,
    pub source: InstallSource,
    #[serde(default)]
    pub engines: BTreeMap<String, String>,
}

/// 缓存完整的安装快照，避免目录刷新改变已选择版本的来源。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexPlugin {
    pub name: String,
    pub title: Option<LocalizedText>,
    pub description: Option<LocalizedText>,
    pub icon: Option<String>,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub repository: Option<Value>,
    #[serde(default)]
    pub metadata: Value,
    pub versions: Vec<IndexVersion>,
    #[serde(default)]
    pub index_url: String,
}

/// 校验官方索引并保留每个版本的来源，供缓存与安装流程共同使用。
pub fn parse_index(value: Value, url: &str) -> Result<Vec<MarketPlugin>, String> {
    if value.get("format").and_then(Value::as_u64) != Some(1) {
        return Err("不支持的官方市场 format，要求 1".into());
    }
    let title: LocalizedText =
        serde_json::from_value(value.get("title").cloned().unwrap_or(Value::Null))
            .map_err(|e| format!("官方市场 title: {e}"))?;
    title.validate()?;
    let mut entries: Vec<IndexPlugin> =
        serde_json::from_value(value.get("plugins").cloned().unwrap_or(Value::Null))
            .map_err(|e| format!("官方市场 plugins: {e}"))?;
    let mut names = HashSet::new();
    let mut out = Vec::new();
    for entry in &mut entries {
        let mut validate = || -> Result<(), String> {
            if !names.insert(entry.name.clone()) || entry.versions.is_empty() {
                return Err("包名重复或版本列表为空".into());
            }
            if let Some(title) = &entry.title {
                title.validate()?;
            }
            if let Some(description) = &entry.description {
                description.validate()?;
            }
            let mut versions = HashSet::new();
            for version in &entry.versions {
                version.source.install_spec(&entry.name, &version.version)?;
                if !versions.insert(&version.version)
                    || version.publish_timestamp >= 100_000_000_000
                {
                    return Err("版本重复或发布时间超出范围".into());
                }
                if version.engines.get("dsh").is_some_and(|range| {
                    range.trim().is_empty() || node_semver::Range::parse(range).is_err()
                }) {
                    return Err(format!(
                        "版本 {} 的 engines.dsh 必须是非空 npm SemVer 范围",
                        version.version
                    ));
                }
            }
            Ok(())
        };
        validate().map_err(|e| format!("官方市场插件 {}: {e}", entry.name))?;
        entry.index_url = url.to_string();
        if let Some(icon) = &entry.icon {
            if !icon.starts_with("data:") {
                let base = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
                let resolved = base.join(icon).map_err(|e| e.to_string())?;
                if resolved.scheme() != "https" && resolved.origin() != base.origin() {
                    return Err(format!(
                        "插件 {} 的图标必须使用 HTTPS 或同源地址",
                        entry.name
                    ));
                }
                entry.icon = Some(resolved.to_string());
            }
        }
        entry.versions.sort_by(|a, b| {
            semver::Version::parse(&b.version)
                .unwrap()
                .cmp_precedence(&semver::Version::parse(&a.version).unwrap())
        });
        let repository = entry
            .repository
            .as_ref()
            .and_then(|r| r.get("url"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        out.push(MarketPlugin {
            id: entry.name.clone(),
            name: entry
                .title
                .clone()
                .map(LocalizedText::normalized)
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| LocalizedText::Text(entry.name.clone())),
            description: entry.description.clone().map(LocalizedText::description),
            support_versions: None,
            urls: Some(MarketPluginUrls {
                homepage: entry.homepage.clone(),
                repository,
                issues: None,
            }),
            relationship: None,
            source: String::new(),
            confidence: Default::default(),
            sources: Vec::new(),
            repo: None,
            verification: None,
            category: entry
                .metadata
                .get("category")
                .and_then(Value::as_str)
                .map(str::to_owned),
            stars: None,
            downloads: None,
            catalog: Some(entry.clone()),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    #[ignore = "需要通过 DSH_TEST_MARKET_INDEX 指定生成的完整索引"]
    fn generated_index_is_installable() {
        let path = std::env::var("DSH_TEST_MARKET_INDEX").expect("缺少索引路径");
        let value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let entries = parse_index(value, "https://dsh-plug.in/api/dsh-plugins.json").unwrap();
        assert!(!entries.is_empty());
        for entry in entries {
            let catalog = entry.catalog.unwrap();
            for version in catalog.versions {
                assert!(version
                    .source
                    .install_spec(&catalog.name, &version.version)
                    .is_ok());
                assert!(version.engines.contains_key("dsh"));
            }
        }
    }

    fn index() -> Value {
        json!({"format": 1, "title": "foo", "plugins": [{
            "name": "@foo/bar", "title": {"en": "Foo", "zh-cn": "示例"}, "icon": "icons/foo.png",
            "versions": [{"version": "1.0.0", "publishTimestamp": 100, "source": {"type": "npm", "registry": "https://example.com/"}, "engines": {"dsh": ">=0.1.0 <0.2.0"}}]
        }]})
    }

    #[test]
    fn catalog_preserves_installation_and_localized_metadata() {
        let entries = parse_index(index(), "https://example.com/api/index.json").unwrap();
        let catalog = entries[0].catalog.as_ref().unwrap();
        assert_eq!(entries[0].id, "@foo/bar");
        assert_eq!(
            catalog.icon.as_deref(),
            Some("https://example.com/api/icons/foo.png")
        );
        assert_eq!(
            catalog.versions[0].source.registry(),
            Some("https://example.com/")
        );
        assert_eq!(catalog.versions[0].engines["dsh"], ">=0.1.0 <0.2.0");
    }

    #[test]
    fn localized_title_becomes_the_plugin_name() {
        let entries = parse_index(index(), "https://example.com/api/index.json").unwrap();
        assert_eq!(
            entries[0].name,
            LocalizedText::Languages(BTreeMap::from([
                ("en".to_string(), "Foo".to_string()),
                ("zh-cn".to_string(), "示例".to_string()),
            ]))
        );

        // A single-language title keeps the string form.
        let mut plain = index();
        plain["plugins"][0]["title"] = json!("Plain");
        let entries = parse_index(plain, "https://example.com/api/index.json").unwrap();
        assert_eq!(entries[0].name, LocalizedText::Text("Plain".into()));

        // A missing title falls back to the package name so the market never
        // renders an empty row.
        let mut untitled = index();
        untitled["plugins"][0]
            .as_object_mut()
            .unwrap()
            .remove("title");
        let entries = parse_index(untitled, "https://example.com/api/index.json").unwrap();
        assert_eq!(entries[0].name, LocalizedText::Text("@foo/bar".into()));
    }

    #[test]
    fn git_installation_pins_commit_and_package_directory() {
        let source = InstallSource::Git {
            url: "https://github.com/foo/bar.git".into(),
            commit: "a".repeat(40),
            path: Some("packages/foo".into()),
        };
        assert_eq!(
            source.install_spec("foo", "1.0.0").unwrap(),
            format!(
                "git+https://github.com/foo/bar.git#{}&path:/packages/foo",
                "a".repeat(40)
            )
        );
    }

    #[test]
    fn invalid_catalog_or_install_source_is_rejected() {
        let mut duplicate = index();
        let entry = duplicate["plugins"][0].clone();
        duplicate["plugins"].as_array_mut().unwrap().push(entry);
        assert!(parse_index(duplicate, "https://example.com/index.json").is_err());
        for source in [
            InstallSource::Npm {
                registry: Some("https://user:pass@example.com".into()),
            },
            InstallSource::Git {
                url: "https://github.com/foo/bar".into(),
                commit: "main".into(),
                path: None,
            },
            InstallSource::Git {
                url: "https://github.com/foo/bar".into(),
                commit: "a".repeat(40),
                path: Some("../foo".into()),
            },
        ] {
            assert!(source.install_spec("foo", "1.0.0").is_err());
        }
    }

    #[test]
    fn official_engine_ranges_use_npm_syntax() {
        for range in ["garbage", "[2.0.0,1.0.0]", "workspace:*", ""] {
            let mut value = index();
            value["plugins"][0]["versions"][0]["engines"]["dsh"] = json!(range);
            let error = parse_index(value, "https://example.com/index.json").unwrap_err();
            assert!(error.contains("@foo/bar"));
            assert!(error.contains("1.0.0"));
            assert!(error.contains("engines.dsh"));
        }
        for range in ["^0.1.0 || >=0.2.1-rc.4", "0.1.0 - 0.1.9", "~1.2", "*"] {
            let mut value = index();
            value["plugins"][0]["versions"][0]["engines"]["dsh"] = json!(range);
            assert!(parse_index(value, "https://example.com/index.json").is_ok());
        }
    }
}
