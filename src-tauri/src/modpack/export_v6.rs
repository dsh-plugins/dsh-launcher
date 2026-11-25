use super::{TmpDir, MODPACK_MAX_BYTES};
use crate::{config::DshHome, config::DshInstance, AppState};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tauri::State;
use tokio::io::AsyncReadExt;

#[derive(Clone, Deserialize)]
#[serde(default)]
pub(crate) struct ExportContents {
    patch: bool,
    lockfile: bool,
    workspace: bool,
    icon: bool,
    extra_files: bool,
    agents_md: bool,
    presets: bool,
    home_extra_files: bool,
    skills: Vec<String>,
}

impl Default for ExportContents {
    fn default() -> Self {
        Self {
            patch: true,
            lockfile: true,
            workspace: true,
            icon: true,
            extra_files: false,
            agents_md: false,
            presets: false,
            home_extra_files: false,
            skills: Vec::new(),
        }
    }
}

impl ExportContents {
    fn worker_flags(&self) -> Value {
        json!({
            "patch": self.patch,
            "lockfile": self.lockfile,
            "workspace": self.workspace,
            "extraFiles": self.extra_files,
            "agentsMd": self.agents_md,
            "presets": self.presets,
            "homeExtraFiles": self.home_extra_files,
        })
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(crate) struct HomeContents {
    agents_md: bool,
    presets: bool,
    extra_files: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum LocalizedText {
    Text(String),
    Translations(BTreeMap<String, String>),
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ExportMetadata {
    name: Option<String>,
    version: Option<String>,
    display_name: Option<LocalizedText>,
    description: Option<LocalizedText>,
    author: Option<String>,
    dsh_version: Option<String>,
    dsh_versions: Vec<String>,
    selection: ExportSelection,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct ExportSelection {
    paths: Vec<ResourceSelection>,
    providers: Vec<ResourceSelection>,
    mcp: Vec<ResourceSelection>,
    skills: Vec<ResourceSelection>,
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum ResourceSelection {
    Key(String),
    Options(ResourceOptions),
}

impl ResourceSelection {
    fn key(&self) -> &str {
        match self {
            Self::Key(key) => key,
            Self::Options(options) => &options.key,
        }
    }
}

#[derive(Deserialize, Serialize)]
struct ResourceOptions {
    key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<LocalizedText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    desc: Option<LocalizedText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<SkillMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subdir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    r#ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    commit: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum SkillMode {
    Embedded,
    Fixed,
    Track,
}

#[derive(Deserialize, Serialize)]
struct PluginOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<LocalizedText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    desc: Option<LocalizedText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    source: Option<Option<PluginSource>>,
}

fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum PluginSource {
    Npm { registry: String },
}

type PluginOverrides = BTreeMap<String, PluginOverride>;

fn validate_plugin_sources(plugins: &PluginOverrides) -> Result<(), String> {
    for (id, plugin) in plugins {
        if plugin.source.is_some()
            && !plugin
                .version
                .as_deref()
                .is_some_and(|version| semver::Version::parse(version).is_ok())
        {
            return Err(format!("插件「{id}」的专用 npm 源必须对应精确版本"));
        }
    }
    Ok(())
}

/// 单配置档导出使用实例及其所属目录，输出位置由保存对话框选定。
#[derive(Deserialize)]
pub(crate) struct ExportModpackInput {
    home_id: String,
    profile: String,
    out_file: String,
    #[serde(default)]
    instance_id: Option<String>,
    #[serde(default)]
    contents: Option<ExportContents>,
    #[serde(default)]
    plugins: PluginOverrides,
    #[serde(flatten)]
    metadata: ExportMetadata,
}

#[derive(Deserialize)]
struct ExportProfileSpec {
    profile: String,
    #[serde(default)]
    contents: Option<ExportContents>,
}

/// 全局目录导出只包含明确选中的配置档与共享资源。
#[derive(Deserialize)]
pub(crate) struct ExportDshhomeInput {
    home_id: String,
    profiles: Vec<ExportProfileSpec>,
    out_file: String,
    #[serde(default)]
    instance_id: Option<String>,
    #[serde(default, rename = "defaultProfile")]
    default_profile: Option<String>,
    #[serde(default = "enabled")]
    icon: bool,
    #[serde(default)]
    home_contents: HomeContents,
    #[serde(default, rename = "profilePlugins")]
    profile_plugins: BTreeMap<String, PluginOverrides>,
    #[serde(flatten)]
    metadata: ExportMetadata,
}

const fn enabled() -> bool {
    true
}

/// 预览只返回可勾选的资源和展示字段，凭据和原始补丁保持在目标机。
#[derive(Deserialize)]
pub(crate) struct InspectPackforgeExportInput {
    home_id: String,
    profiles: Vec<String>,
    whole: bool,
    #[serde(default)]
    instance_id: Option<String>,
    #[serde(default)]
    contents: ExportContents,
    #[serde(default)]
    profile_contents: BTreeMap<String, ExportContents>,
}

struct ExportContext {
    home: DshHome,
    instance: Option<DshInstance>,
    version: Option<String>,
    versions: Vec<String>,
}

impl ExportContext {
    fn resolve(state: &AppState, home_id: &str, instance_id: Option<&str>) -> Result<Self, String> {
        let cfg = state.config.lock().unwrap();
        let home = cfg
            .homes
            .iter()
            .find(|home| home.id == home_id)
            .cloned()
            .ok_or("找不到要导出的 DSH_HOME")?;
        let instance = match instance_id {
            Some(id) => Some(
                cfg.instances
                    .iter()
                    .find(|instance| instance.id == id && instance.home_id == home_id)
                    .cloned()
                    .ok_or("导出实例已变更或不属于所选 DSH_HOME")?,
            ),
            None => cfg
                .instances
                .iter()
                .find(|instance| instance.home_id == home_id)
                .cloned(),
        };
        let installed: Vec<_> = cfg
            .versions
            .iter()
            .filter(|version| version.wsl == home.wsl)
            .collect();
        let versions: BTreeSet<_> = installed
            .iter()
            .filter_map(|version| semver::Version::parse(&version.version).ok())
            .collect();
        let version = instance
            .as_ref()
            .and_then(|instance| {
                installed
                    .iter()
                    .find(|version| version.id == instance.version_id)
            })
            .filter(|version| semver::Version::parse(&version.version).is_ok())
            .map(|version| version.version.clone())
            .or_else(|| versions.last().map(ToString::to_string));
        Ok(Self {
            home,
            instance,
            version,
            versions: versions.iter().rev().map(ToString::to_string).collect(),
        })
    }

    fn worker_request(&self, profiles: &[&str], whole: bool) -> Result<Value, String> {
        validate_profiles(profiles, whole)?;
        let home = self.home.path.to_str().ok_or("DSH_HOME 路径编码无效")?;
        let mut request = json!({
            "kind": if whole { "dshhome" } else { "profile" },
            "home": { "name": self.home.name, "dir": home },
            "exportOptions": {},
        });
        if !whole {
            let name = profiles[0];
            let dir = if self.home.wsl.is_some() {
                format!("{}/profiles/{name}", home.trim_end_matches('/'))
            } else {
                let path = crate::plugins::profile_dir_pub(&self.home.path, name);
                path.to_str().ok_or("配置档路径编码无效")?.to_owned()
            };
            request["profile"] = json!({ "name": name, "dir": dir });
        }
        Ok(request)
    }
}

fn validate_profiles(profiles: &[&str], whole: bool) -> Result<(), String> {
    if profiles.is_empty() || (!whole && profiles.len() != 1) {
        return Err("请选择要导出的配置档".to_owned());
    }
    let mut seen = BTreeSet::new();
    for name in profiles {
        let lower = name.to_lowercase();
        let stem = lower.split('.').next().unwrap_or_default();
        let device = matches!(stem, "con" | "prn" | "aux" | "nul")
            || stem
                .strip_prefix("com")
                .or_else(|| stem.strip_prefix("lpt"))
                .is_some_and(|number| {
                    matches!(
                        number,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                });
        if name.is_empty()
            || name.trim() != *name
            || name.ends_with('.')
            || name
                .chars()
                .any(|c| c.is_control() || "/\\:<>*?|\"".contains(c))
            || device
            || matches!(
                lower.as_str(),
                "__temp__" | "node_modules" | "__proto__" | "prototype" | "constructor"
            )
            || (whole && matches!(lower.as_str(), "web" | "headless"))
            || !seen.insert(lower)
        {
            return Err(format!("配置档「{name}」的名称保留、无效或重复"));
        }
    }
    Ok(())
}

fn text_setting(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

fn runtime_versions(
    preferred: Option<String>,
    versions: Vec<String>,
    current: Option<&str>,
) -> Result<(Option<String>, Vec<String>), String> {
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for version in versions {
        let version = version.trim();
        if semver::Version::parse(version).is_err() {
            return Err("兼容 DSH 版本必须是精确版本号".to_owned());
        }
        if seen.insert(version.to_owned()) {
            selected.push(version.to_owned());
        }
    }
    let explicit = text_setting(preferred);
    if let Some(version) = explicit.as_deref() {
        if semver::Version::parse(version).is_err() {
            return Err("首选 DSH 版本必须是精确版本号".to_owned());
        }
        if !selected.is_empty() && !seen.contains(version) {
            return Err("首选 DSH 版本必须位于兼容版本列表中".to_owned());
        }
    }
    let preferred = explicit
        .or_else(|| selected.first().cloned())
        .or_else(|| current.map(str::to_owned));
    if selected.is_empty() {
        selected.extend(preferred.iter().cloned());
    }
    Ok((preferred, selected))
}

impl ExportMetadata {
    fn worker_options(self, context: &ExportContext, fallback_name: &str) -> Result<Value, String> {
        let (preferred, versions) = runtime_versions(
            self.dsh_version,
            self.dsh_versions,
            context.version.as_deref(),
        )?;
        let name = text_setting(self.name).unwrap_or_else(|| fallback_name.to_owned());
        let version = text_setting(self.version).unwrap_or_else(|| "1.0.0".to_owned());
        if semver::Version::parse(&version).is_err() {
            return Err("整合包版本必须是有效的语义版本号".to_owned());
        }
        let display_name = self.display_name.or_else(|| {
            context
                .instance
                .as_ref()
                .map(|instance| LocalizedText::Text(instance.name.clone()))
        });
        let author = text_setting(self.author).or_else(|| {
            ["USERNAME", "USER"]
                .iter()
                .find_map(|key| text_setting(std::env::var(key).ok()))
        });
        let mut options = json!({
            "name": name,
            "version": version,
            "displayName": display_name,
            "description": self.description,
            "author": author,
            "selection": self.selection,
        });
        if let Some(preferred) = preferred {
            options["dshVersion"] = json!(preferred);
            options["dshVersions"] = json!(versions);
        }
        Ok(options)
    }
}

#[tauri::command]
pub(crate) async fn inspect_packforge_export(
    state: State<'_, AppState>,
    input: InspectPackforgeExportInput,
) -> Result<Value, String> {
    let context = ExportContext::resolve(&state, &input.home_id, input.instance_id.as_deref())?;
    let profiles: Vec<_> = input.profiles.iter().map(String::as_str).collect();
    let mut request = context.worker_request(&profiles, input.whole)?;
    if input.whole {
        request["profiles"] = Value::Object(
            profiles
                .iter()
                .map(|&profile| {
                    let flags = input
                        .profile_contents
                        .get(profile)
                        .cloned()
                        .unwrap_or_default();
                    (profile.to_owned(), flags.worker_flags())
                })
                .collect(),
        );
    } else {
        request["contents"] = input.contents.worker_flags();
    }
    let _snapshot = super::v6::try_home_guard(&state, &context.home)?;
    let mut result = crate::packforge::run(
        &state,
        context.home.wsl.as_deref(),
        "inspectExport",
        request,
    )
    .await?;
    let result_data = result
        .as_object_mut()
        .ok_or("PackForge 返回的导出预览格式无效")?;
    result_data.insert("dsh_version".to_owned(), json!(context.version));
    result_data.insert("dsh_versions".to_owned(), json!(context.versions));
    Ok(result)
}

#[tauri::command]
pub(crate) async fn export_modpack(
    state: State<'_, AppState>,
    input: ExportModpackInput,
) -> Result<String, String> {
    let context = ExportContext::resolve(&state, &input.home_id, input.instance_id.as_deref())?;
    let mut request = context.worker_request(&[&input.profile], false)?;
    validate_plugin_sources(&input.plugins)?;
    let mut metadata = input.metadata;
    let contents = input.contents.unwrap_or_default();
    for entry in &contents.skills {
        if entry.is_empty()
            || entry.contains(['/', '\\', ':'])
            || entry.chars().any(char::is_control)
        {
            return Err("旧版技能选择包含无效条目".to_owned());
        }
        let key = format!("skill:home:{entry}");
        if !metadata
            .selection
            .skills
            .iter()
            .any(|item| item.key() == key)
        {
            metadata.selection.skills.push(ResourceSelection::Key(key));
        }
    }
    let mut options = metadata.worker_options(&context, &input.profile)?;
    options["profileName"] = json!(input.profile);
    options["plugins"] = json!(input.plugins);
    request["contents"] = contents.worker_flags();
    request["exportOptions"] = options;
    export(&state, context, request, &input.out_file, contents.icon).await
}

#[tauri::command]
pub(crate) async fn export_dshhome_modpack(
    state: State<'_, AppState>,
    input: ExportDshhomeInput,
) -> Result<String, String> {
    let context = ExportContext::resolve(&state, &input.home_id, input.instance_id.as_deref())?;
    let profiles: Vec<_> = input
        .profiles
        .iter()
        .map(|spec| spec.profile.as_str())
        .collect();
    let mut request = context.worker_request(&profiles, true)?;
    if input
        .profile_plugins
        .keys()
        .any(|profile| !profiles.contains(&profile.as_str()))
    {
        return Err("插件展示设置对应未选中的配置档".to_owned());
    }
    for plugins in input.profile_plugins.values() {
        validate_plugin_sources(plugins)?;
    }
    let preferred = input.default_profile.as_deref();
    if preferred.is_some_and(|name| !profiles.contains(&name)) {
        return Err("默认配置档必须位于已选配置档列表中".to_owned());
    }
    let default_profile = preferred
        .or_else(|| {
            context
                .instance
                .as_ref()
                .and_then(|item| item.default_profile.as_deref())
                .filter(|name| profiles.contains(name))
        })
        .unwrap_or(profiles[0]);
    let mut options = input.metadata.worker_options(&context, "dsh-home-pack")?;
    options["defaultProfile"] = json!(default_profile);
    options["profilePlugins"] = json!(input.profile_plugins);
    request["profiles"] = Value::Object(
        input
            .profiles
            .into_iter()
            .map(|spec| {
                (
                    spec.profile,
                    spec.contents.unwrap_or_default().worker_flags(),
                )
            })
            .collect(),
    );
    request["homeContents"] = json!({
        "agentsMd": input.home_contents.agents_md,
        "presets": input.home_contents.presets,
        "extraFiles": input.home_contents.extra_files,
    });
    request["exportOptions"] = options;
    export(&state, context, request, &input.out_file, input.icon).await
}

struct ExportScratch {
    worker: String,
    local: TmpDir,
}

impl ExportScratch {
    async fn create(state: &State<'_, AppState>, distro: Option<&str>) -> Result<Self, String> {
        let name = format!("export-{}", uuid::Uuid::new_v4().simple());
        let (worker, local) = if let Some(distro) = distro {
            crate::wsl::ensure_distro_running(state, distro).await?;
            let base = crate::wsl::WslRoot::resolve(distro).await?.0;
            let parent = format!("{base}/packforge");
            let folder = format!("{parent}/{name}");
            crate::wsl::wsl_output(
                distro,
                &[
                    "/bin/mkdir".to_owned(),
                    "-p".to_owned(),
                    "--".to_owned(),
                    parent,
                ],
            )
            .await?;
            crate::wsl::wsl_output(
                distro,
                &[
                    "/bin/mkdir".to_owned(),
                    "-m".to_owned(),
                    "700".to_owned(),
                    "--".to_owned(),
                    folder.clone(),
                ],
            )
            .await?;
            let local = crate::wsl::unc_path(distro, &folder);
            (folder, local)
        } else {
            let parent = state.data_dir.join("packforge");
            std::fs::create_dir_all(&parent).map_err(|e| format!("创建导出工作目录失败：{e}"))?;
            let local = parent.join(name);
            let worker = local.to_str().ok_or("导出工作目录编码无效")?.to_owned();
            let dir = std::fs::DirBuilder::new();
            #[cfg(unix)]
            let dir = {
                use std::os::unix::fs::DirBuilderExt;
                let mut dir = dir;
                dir.mode(0o700);
                dir
            };
            dir.create(&local)
                .map_err(|e| format!("创建独立导出目录失败：{e}"))?;
            (worker, local)
        };
        Ok(Self {
            worker,
            local: TmpDir(local),
        })
    }

    fn worker_file(&self, name: &str, distro: Option<&str>) -> Result<String, String> {
        if distro.is_some() {
            Ok(format!("{}/{name}", self.worker))
        } else {
            self.local
                .0
                .join(name)
                .to_str()
                .map(str::to_owned)
                .ok_or("导出文件路径编码无效".to_owned())
        }
    }
}

async fn export(
    state: &State<'_, AppState>,
    context: ExportContext,
    mut request: Value,
    out_file: &str,
    include_icon: bool,
) -> Result<String, String> {
    let output = output_path(out_file)?;
    let distro = context.home.wsl.as_deref();
    let scratch = ExportScratch::create(state, distro).await?;
    let snapshot = super::v6::try_home_guard(state, &context.home)?;
    request["output"] = json!(scratch.worker_file("archive.dspack", distro)?);
    if include_icon {
        match instance_icon(&context).await {
            Some(ExportIcon::Bytes(bytes)) => {
                let icon = scratch.local.0.join("icon.png");
                tokio::fs::write(&icon, bytes)
                    .await
                    .map_err(|e| format!("暂存整合包图标失败：{e}"))?;
                request["iconFile"] = json!(scratch.worker_file("icon.png", distro)?);
            }
            Some(ExportIcon::Url(url)) => {
                request["exportOptions"]["icon"] = json!(url.as_str());
            }
            None => {}
        }
    }
    let result = crate::packforge::run(state, distro, "export", request).await?;
    drop(snapshot);
    let size = result["size"]
        .as_u64()
        .ok_or("PackForge 导出结果缺少归档大小")?;
    let sha256 = result["sha256"]
        .as_str()
        .ok_or("PackForge 导出结果缺少 SHA-256")?
        .to_owned();
    crate::wsl::run_blocking(move || {
        publish(
            &scratch.local.0.join("archive.dspack"),
            &output,
            size,
            &sha256,
        )?;
        crate::log_info!("已导出 PackForge 整合包 {}", output.display());
        Ok(output.to_string_lossy().into_owned())
    })
    .await?
}

enum ExportIcon {
    Bytes(Vec<u8>),
    Url(reqwest::Url),
}

async fn instance_icon(context: &ExportContext) -> Option<ExportIcon> {
    let instance = context.instance.as_ref()?;
    let icon = instance.icon.as_deref()?;
    if icon == "local" {
        let home = crate::wsl::home_fs_path(&context.home);
        let path = crate::icons::local_icon_path(&home, &instance.id);
        let file = tokio::fs::File::open(path).await.ok()?;
        let mut bytes = Vec::new();
        let limit = 16 * 1024 * 1024;
        file.take(limit + 1).read_to_end(&mut bytes).await.ok()?;
        return (!bytes.is_empty() && bytes.len() as u64 <= limit)
            .then_some(ExportIcon::Bytes(bytes));
    }
    let url = reqwest::Url::parse(icon).ok()?;
    if !matches!(url.scheme(), "https" | "http")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    match crate::icons::fetch_square_icon_png(url.as_str()).await {
        Ok(bytes) => Some(ExportIcon::Bytes(bytes)),
        Err(_) if url.scheme() == "https" && url.query().is_none() && url.fragment().is_none() => {
            Some(ExportIcon::Url(url))
        }
        Err(_) => None,
    }
}

fn output_path(raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    let mut path = PathBuf::from(raw);
    if !path.is_absolute() || path.file_name().is_none() {
        return Err("请选择完整的整合包保存路径".to_owned());
    }
    if !raw.to_ascii_lowercase().ends_with(".dspack") {
        path = PathBuf::from(format!("{raw}.dspack"));
    }
    Ok(path)
}

struct RemoveTempFile(PathBuf);

impl Drop for RemoveTempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn publish(source: &Path, target: &Path, size: u64, sha256: &str) -> Result<(), String> {
    if size == 0 || size > MODPACK_MAX_BYTES as u64 {
        return Err(format!(
            "导出的整合包大小超出支持限制（{} MiB）",
            MODPACK_MAX_BYTES / (1024 * 1024)
        ));
    }
    let source_info =
        std::fs::symlink_metadata(source).map_err(|e| format!("读取导出归档失败：{e}"))?;
    if !source_info.is_file() || source_info.len() != size {
        return Err("导出归档与引擎返回的大小不符".to_owned());
    }
    let parent = target.parent().ok_or("整合包保存目录无效")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("创建整合包保存目录失败：{e}"))?;
    let mut input = std::fs::File::open(source)
        .map_err(|e| format!("打开导出归档失败：{e}"))?
        .take(size + 1);
    let temporary = parent.join(format!(".dspack-{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|e| format!("创建整合包临时文件失败：{e}"))?;
    let _guard = RemoveTempFile(temporary.clone());
    let mut bytes = 0;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|e| format!("读取导出归档失败：{e}"))?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        hash.update(&buffer[..count]);
        file.write_all(&buffer[..count])
            .map_err(|e| format!("写入整合包失败：{e}"))?;
    }
    if bytes != size || format!("{:x}", hash.finalize()) != sha256 {
        return Err("导出归档 SHA-256 校验失败".to_owned());
    }
    file.sync_all()
        .map_err(|e| format!("保存整合包内容失败：{e}"))?;
    drop(file);
    std::fs::rename(&temporary, target).map_err(|e| format!("提交整合包文件失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_versions_keep_the_selected_preference() {
        let selected = vec![
            "0.2.3-alpha.2".to_owned(),
            "0.2.4".to_owned(),
            "0.2.4".to_owned(),
        ];
        let (preferred, versions) =
            runtime_versions(None, selected.clone(), Some("0.3.0")).unwrap();
        assert_eq!(preferred.as_deref(), Some("0.2.3-alpha.2"));
        assert_eq!(versions, ["0.2.3-alpha.2", "0.2.4"]);
        assert!(runtime_versions(Some("0.3.0".into()), selected, None).is_err());
        assert!(runtime_versions(None, vec!["^0.2.3".into()], None).is_err());
    }

    #[test]
    fn export_profile_names_are_portable_and_distinct() {
        validate_profiles(&["web"], false).unwrap();
        validate_profiles(&["中文", "Foo"], true).unwrap();
        assert!(validate_profiles(&["web"], true).is_err());
        assert!(validate_profiles(&["Profile", "profile"], true).is_err());
        assert!(validate_profiles(&["../foo"], false).is_err());
        assert!(validate_profiles(&["CON.txt"], false).is_err());
        assert!(validate_profiles(&["__proto__"], true).is_err());
    }

    #[test]
    fn preserves_the_difference_between_clearing_and_leaving_a_plugin_source() {
        let unset: PluginOverride = serde_json::from_value(json!({"version":"1.0.0"})).unwrap();
        let clear: PluginOverride =
            serde_json::from_value(json!({"version":"1.0.0", "source":null})).unwrap();
        assert!(serde_json::to_value(unset).unwrap().get("source").is_none());
        assert!(serde_json::to_value(clear)
            .unwrap()
            .get("source")
            .is_some_and(Value::is_null));
    }

    #[test]
    fn export_options_accept_the_shared_compatibility_fields() {
        let input: ExportDshhomeInput = serde_json::from_value(json!({
            "home_id":"home",
            "profiles":[{"profile":"foo", "contents":null}],
            "out_file":"foo.dspack",
            "defaultProfile":"foo",
            "dshVersion":"0.2.3-alpha.2",
            "dshVersions":["0.2.3-alpha.2","0.2.3"],
            "profilePlugins":{"foo":{"@bar/baz":{"version":"1.0.0","source":{"type":"npm","registry":"https://example.test/"}}}},
            "selection":{"skills":[{"key":"skill:home:foo", "mode":"embedded"}]},
        })).unwrap();
        assert_eq!(input.default_profile.as_deref(), Some("foo"));
        assert_eq!(input.metadata.dsh_version.as_deref(), Some("0.2.3-alpha.2"));
        assert_eq!(input.metadata.dsh_versions.len(), 2);
        assert_eq!(input.metadata.selection.skills[0].key(), "skill:home:foo");
        assert!(input.profiles[0].contents.is_none());
        validate_plugin_sources(&input.profile_plugins["foo"]).unwrap();
    }

    #[test]
    fn failed_archive_checksum_preserves_previous_output() {
        let scratch =
            TmpDir(std::env::temp_dir().join(format!("export-{}", uuid::Uuid::new_v4().simple())));
        std::fs::create_dir(&scratch.0).unwrap();
        let input = scratch.0.join("archive.partial");
        let output = scratch.0.join("archive.dspack");
        std::fs::write(&input, b"new content").unwrap();
        std::fs::write(&output, b"previous content").unwrap();
        assert!(publish(&input, &output, 11, &"0".repeat(64)).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"previous content");
        publish(
            &input,
            &output,
            11,
            &format!("{:x}", Sha256::digest(b"new content")),
        )
        .unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"new content");
    }
}
