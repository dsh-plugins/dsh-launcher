use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Persistent configuration models
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DshHome {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    /// WSL distro name when this HOME lives inside WSL (issue #19): `path`
    /// then holds a Linux path string inside that distro. `None` = local
    /// Windows HOME.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wsl: Option<String>,
    /// Storage redirections (issue #51): whitelisted entry name → absolute
    /// target path. The entry inside this HOME is replaced by a link to the
    /// target, so bulky data (sessions, attachments, …) can live elsewhere.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub links: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DshVersion {
    pub id: String,
    pub version: String,
    pub dir: PathBuf,
    /// WSL distro this version is installed into (issue #19); `dir` is then
    /// a Linux path inside the distro. `None` = local Windows install.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wsl: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DshInstance {
    pub id: String,
    pub name: String,
    pub version_id: String,
    pub home_id: String,
    #[serde(default)]
    pub env_overrides: BTreeMap<String, String>,
    #[serde(default)]
    pub default_profile: Option<String>,
    #[serde(default)]
    pub last_profile: Option<String>,
    /// Instance icon: an http(s) URL, or "local" for a cropped PNG stored at
    /// `<home>/icons/<id>.png`. `None` falls back to the launcher icon.
    #[serde(default)]
    pub icon: Option<String>,
    /// Preferred web port (issue #21): `Some(1-65535)` pins it; `None` binds
    /// a random free port (`--port 0`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

// ---------------------------------------------------------------------------
// Plugin catalog sources (issue #46)
// ---------------------------------------------------------------------------

/// How a configured plugin source is fetched/parsed. The launcher dispatches
/// to one adapter per kind.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    /// dsh-plug.in's native schema (array of MarketPlugin).
    Primary,
    /// awesome-dsh-plugin.com's schema (plugins[] with an `install` line).
    Awesome,
    /// DSH Get's aggregated catalog (plugins[] with an `install` line).
    DshGet,
    /// GitHub `topic:dsh-plugin` search (live discovery, no static URL).
    GithubTopic,
}

/// How trustworthy a source is. Ordered from least to most trustworthy so
/// duplicate ids can keep the highest tier.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// Live/community discovery; the code has not been reviewed.
    #[default]
    Unverified,
    /// Third-party directory that aggregates other catalogs.
    Aggregated,
    /// Curated community directory (PR gate + CI validation).
    Curated,
    /// The official dsh-plug.in catalog.
    Official,
}

/// One configured plugin catalog source. Persisted in launcher settings so the
/// user can enable/disable, reorder, and add mirrors or private catalogs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginSourceConfig {
    /// Stable id; also stamped onto every entry as `MarketPlugin.source`.
    pub id: String,
    /// Catalog JSON URL (http/https). Empty for live-only kinds.
    pub url: String,
    pub kind: SourceKind,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub confidence: Confidence,
    #[serde(default)]
    pub order: u32,
}

/// The built-in catalog sources, in display/dedup priority order. The live
/// GitHub topic channel ships disabled: it is rate-limited and unverified, so
/// the user opts in from Settings.
pub fn default_plugin_sources() -> Vec<PluginSourceConfig> {
    vec![
        PluginSourceConfig {
            id: "dsh-plugins".to_string(),
            url: "https://dsh-plug.in/api/plugins.json".to_string(),
            kind: SourceKind::Primary,
            enabled: true,
            confidence: Confidence::Official,
            order: 0,
        },
        PluginSourceConfig {
            id: "awesome-dsh-plugin".to_string(),
            url: "https://awesome-dsh-plugin.com/plugins.json".to_string(),
            kind: SourceKind::Awesome,
            enabled: true,
            confidence: Confidence::Curated,
            order: 1,
        },
        PluginSourceConfig {
            id: "dshget".to_string(),
            // Served through the jsDelivr CDN: raw.githubusercontent.com is
            // unreachable on some networks (observed in CN), while the CDN
            // mirror serves the identical snapshot. Users can point this at
            // raw.githubusercontent.com or another mirror in Settings.
            url: "https://cdn.jsdelivr.net/gh/bobby-sheng/dshget-data@main/catalog.json"
                .to_string(),
            kind: SourceKind::DshGet,
            enabled: true,
            confidence: Confidence::Aggregated,
            order: 2,
        },
        PluginSourceConfig {
            id: "github-topic".to_string(),
            url: String::new(),
            kind: SourceKind::GithubTopic,
            enabled: false,
            confidence: Confidence::Unverified,
            order: 3,
        },
    ]
}

/// Returns the built-in definition for a source id, when it is one of the
/// launcher-shipped catalogs.
fn builtin_source(id: &str) -> Option<PluginSourceConfig> {
    default_plugin_sources().into_iter().find(|s| s.id == id)
}

/// Drops user-supplied source entries that cannot be driven (empty id, or a
/// non-http(s) URL for a static kind), de-duplicates ids, and renumbers `order`
/// to the list index so the persisted list is always a coherent priority order.
///
/// Trust is launcher-assigned, never self-attested (issue #46 review): entries
/// reusing a built-in id keep their customized URL/enabled state but have
/// `kind`/`confidence` restored to the built-in values, and custom sources are
/// always forced to `Confidence::Unverified` no matter what the payload claims
/// — otherwise a custom catalog could attest itself "official", overwrite the
/// official entry's repo hint in dedup, and bypass the unverified-install ack.
pub fn sanitize_plugin_sources(list: Vec<PluginSourceConfig>) -> Vec<PluginSourceConfig> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    list.into_iter()
        .filter(|s| {
            let id = s.id.trim().to_string();
            let url = s.url.trim();
            !id.is_empty()
                && (s.kind == SourceKind::GithubTopic
                    || url.starts_with("https://")
                    || url.starts_with("http://"))
                && seen.insert(id)
        })
        .enumerate()
        .map(|(i, mut s)| {
            s.id = s.id.trim().to_string();
            s.url = s.url.trim().to_string();
            s.order = i as u32;
            match builtin_source(&s.id) {
                Some(builtin) => {
                    s.kind = builtin.kind;
                    s.confidence = builtin.confidence;
                }
                None => {
                    s.confidence = Confidence::Unverified;
                }
            }
            s
        })
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LauncherSettings {
    #[serde(default = "default_locale")]
    pub locale: String,
    #[serde(default = "default_true")]
    pub minimize_to_tray: bool,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub last_instance_id: Option<String>,
    /// News feed source: an http(s) URL or a local .md/.html file path.
    #[serde(default = "default_news_source")]
    pub news_source: String,
    /// UI theme: "light" | "dark" | "system" (follow the OS setting).
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Runtime log level: "debug" | "info" | "warn" | "error".
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// SKILL source repositories (issue #10):
    /// `https://[user:password@]github.com/user/repo[.git][#/path/to/skill]`.
    #[serde(default)]
    pub skill_repos: Vec<String>,
    /// Route the launcher's own HTTP requests through a proxy.
    #[serde(default)]
    pub proxy_enabled: bool,
    /// Proxy URL without port, e.g. `http://127.0.0.1`.
    #[serde(default = "default_proxy_url")]
    pub proxy_url: String,
    #[serde(default = "default_proxy_port")]
    pub proxy_port: u16,
    /// Comma-separated hosts that bypass the proxy (NO_PROXY).
    #[serde(default = "default_no_proxy")]
    pub no_proxy: String,
    /// Also inject the proxy into launched dsh instances, overriding the
    /// instance's own environment variables (applies on next start).
    #[serde(default)]
    pub proxy_apply_dsh: bool,
    /// Automatically open the instance window after a successful launch from
    /// the Home page (waits for the web URL to become ready).
    #[serde(default = "default_true")]
    pub auto_open_on_launch: bool,
    /// Hide the launcher main window whenever an instance window (web GUI or
    /// TUI terminal) is opened/focused.
    #[serde(default)]
    pub hide_launcher_on_window_open: bool,
    /// Plugin marketplace catalog sources (issue #46). Defaults to the built-in
    /// three catalogs plus the (disabled) live GitHub topic channel.
    #[serde(default = "default_plugin_sources")]
    pub plugin_sources: Vec<PluginSourceConfig>,
    /// Root directory of the most recent storage redirection (issue #65),
    /// offered as the "last used" preset. Advisory UI memory only — it never
    /// affects validation. `#[serde(default)]` keeps older configs loadable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_link_root: Option<String>,
}

fn default_locale() -> String {
    "zh-CN".to_string()
}

fn default_true() -> bool {
    true
}

fn default_theme() -> String {
    "system".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_news_source() -> String {
    "https://gist.githubusercontent.com/Gu-ZT/f08daa33afb82f4b375e604039b92742/raw/DSH_NEWS.md"
        .to_string()
}

fn default_proxy_url() -> String {
    "http://127.0.0.1".to_string()
}

fn default_proxy_port() -> u16 {
    7890
}

fn default_no_proxy() -> String {
    "127.0.0.1,localhost,::1".to_string()
}

impl Default for LauncherSettings {
    fn default() -> Self {
        Self {
            locale: default_locale(),
            minimize_to_tray: default_true(),
            autostart: false,
            last_instance_id: None,
            news_source: default_news_source(),
            theme: default_theme(),
            log_level: default_log_level(),
            skill_repos: Vec::new(),
            proxy_enabled: false,
            proxy_url: default_proxy_url(),
            proxy_port: default_proxy_port(),
            no_proxy: default_no_proxy(),
            proxy_apply_dsh: false,
            auto_open_on_launch: true,
            hide_launcher_on_window_open: false,
            plugin_sources: default_plugin_sources(),
            last_link_root: None,
        }
    }
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub homes: Vec<DshHome>,
    #[serde(default)]
    pub versions: Vec<DshVersion>,
    #[serde(default)]
    pub instances: Vec<DshInstance>,
    #[serde(default)]
    pub settings: LauncherSettings,
}

// ---------------------------------------------------------------------------
// API / event payloads (mirrored by the frontend)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteVersion {
    pub version: String,
    pub released_at: Option<String>,
    /// Where the version comes from: absent/`npm` installs from the registry;
    /// `github` marks a GitHub-only tag (dsh-v*) that must be built from
    /// source (clone + pnpm install + build).
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewInstanceInput {
    pub name: String,
    pub version_id: String,
    pub home_id: String,
    #[serde(default)]
    pub env_overrides: BTreeMap<String, String>,
    #[serde(default)]
    pub default_profile: Option<String>,
}

/// Partial settings update: only present fields are applied.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SettingsPatch {
    #[serde(default)]
    pub locale: Option<String>,
    #[serde(default)]
    pub minimize_to_tray: Option<bool>,
    #[serde(default)]
    pub autostart: Option<bool>,
    #[serde(default)]
    pub last_instance_id: Option<String>,
    #[serde(default)]
    pub news_source: Option<String>,
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub log_level: Option<String>,
    #[serde(default)]
    pub skill_repos: Option<Vec<String>>,
    #[serde(default)]
    pub proxy_enabled: Option<bool>,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub proxy_port: Option<u16>,
    #[serde(default)]
    pub no_proxy: Option<String>,
    #[serde(default)]
    pub proxy_apply_dsh: Option<bool>,
    #[serde(default)]
    pub plugin_sources: Option<Vec<PluginSourceConfig>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InstanceState {
    Stopped,
    Starting,
    Running,
    Exited,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstanceStatus {
    pub id: String,
    pub state: InstanceState,
    pub url: Option<String>,
    pub profile: Option<String>,
    pub exit_code: Option<i32>,
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

pub fn load_config(path: &Path) -> Config {
    match fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<Config>(&raw) {
            Ok(mut cfg) => {
                dedupe_homes(&mut cfg);
                cleanup_orphan_homes(&mut cfg);
                ensure_user_dsh_home(&mut cfg);
                migrate_news_source(&mut cfg);
                // Self-heal configs persisted before trust locking existed:
                // strip self-attested confidence/kind from stored sources.
                cfg.settings.plugin_sources =
                    sanitize_plugin_sources(std::mem::take(&mut cfg.settings.plugin_sources));
                cfg
            }
            Err(err) => {
                // Back up the broken file and start fresh.
                let _ = fs::copy(path, path.with_extension("json.bak"));
                eprintln!("dsh-launcher: config corrupted, backed up: {err}");
                Config::default()
            }
        },
        Err(_) => Config::default(),
    }
}

/// Backfills an empty (previously unset) news_source with the default feed,
/// so existing users also get the default news URL without being forced to
/// change a value they explicitly cleared. An explicitly cleared value is
/// indistinguishable from "never set", so this only runs once when empty.
fn migrate_news_source(cfg: &mut Config) {
    if cfg.settings.news_source.trim().is_empty() {
        cfg.settings.news_source = default_news_source();
    }
}

/// Removes HOME records whose directory no longer exists AND that are not
/// referenced by any instance (a stale placeholder from an interrupted task
/// or a manually deleted folder). Directories that still exist are kept even
/// if unreferenced (they may be user-managed).
pub fn cleanup_orphan_homes(cfg: &mut Config) {
    let orphans: Vec<String> = cfg
        .homes
        .iter()
        .filter(|h| !h.path.exists() && !cfg.instances.iter().any(|i| i.home_id == h.id))
        .map(|h| h.id.clone())
        .collect();
    if orphans.is_empty() {
        return;
    }
    cfg.homes.retain(|h| !orphans.contains(&h.id));
}

/// If the user's home directory contains a `.dsh` folder, make sure a HOME
/// record points at it so it can be picked as a DSH_HOME and referenced by
/// instances. Idempotent.
pub fn ensure_user_dsh_home(cfg: &mut Config) {
    let home_dir = std::env::var("USERPROFILE")
        .ok()
        .or_else(|| std::env::var("HOME").ok())
        .map(std::path::PathBuf::from);
    let Some(home_dir) = home_dir else { return };
    let dsh = home_dir.join(".dsh");
    if !dsh.exists() {
        return;
    }
    if cfg.homes.iter().any(|h| paths_equal(&h.path, &dsh)) {
        return;
    }
    cfg.homes.push(DshHome {
        id: "home-user-dsh".to_string(),
        name: "用户默认 (~/.dsh)".to_string(),
        path: dsh,
        wsl: None,
        links: Default::default(),
    });
}

/// Case-insensitive path equality (Windows filesystems are case-insensitive).
pub fn paths_equal(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// Merge HOME records that point at the same path, keeping the first and
/// redirecting instance references to the surviving home id (cleans up
/// duplicates created before path-based reuse existed).
pub fn dedupe_homes(cfg: &mut Config) {
    if cfg.homes.len() < 2 {
        return;
    }
    let mut kept: Vec<DshHome> = Vec::new();
    let mut redirect: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for home in &cfg.homes {
        if let Some(existing) = kept.iter().find(|e| paths_equal(&e.path, &home.path)) {
            redirect.insert(home.id.clone(), existing.id.clone());
        } else {
            kept.push(home.clone());
        }
    }
    if redirect.is_empty() {
        return;
    }
    for inst in &mut cfg.instances {
        if let Some(new_id) = redirect.get(&inst.home_id) {
            inst.home_id = new_id.clone();
        }
    }
    cfg.homes = kept;
}

pub fn save_config(path: &Path, cfg: &Config) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, raw).map_err(|e| format!("写入配置失败: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| format!("保存配置失败: {e}"))?;
    Ok(())
}

pub fn sanitize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_alphanumeric() || ch == '-' || ch == '_' || ch == '.' || ch == ' ' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim().to_string();
    if trimmed.is_empty() {
        "instance".to_string()
    } else {
        trimmed
    }
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(id: &str, kind: SourceKind, confidence: Confidence) -> PluginSourceConfig {
        PluginSourceConfig {
            id: id.to_string(),
            url: "https://example.com/catalog.json".to_string(),
            kind,
            enabled: true,
            confidence,
            order: 99,
        }
    }

    #[test]
    fn sanitize_locks_builtin_kind_and_confidence() {
        // A payload reusing the dshget id but claiming to be an official
        // primary catalog must be reverted to the built-in definition; the
        // customized URL (mirror use case) survives.
        let out = sanitize_plugin_sources(vec![source(
            "dshget",
            SourceKind::Primary,
            Confidence::Official,
        )]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, SourceKind::DshGet);
        assert_eq!(out[0].confidence, Confidence::Aggregated);
        assert_eq!(out[0].url, "https://example.com/catalog.json");
        assert_eq!(out[0].order, 0);
    }

    #[test]
    fn sanitize_forces_custom_sources_unverified() {
        let out = sanitize_plugin_sources(vec![
            source("my-mirror", SourceKind::Primary, Confidence::Official),
            source("corp-hub", SourceKind::Awesome, Confidence::Curated),
        ]);
        assert_eq!(out.len(), 2);
        // Custom sources keep their schema kind (it drives parsing) but never
        // their self-attested trust tier.
        assert_eq!(out[0].kind, SourceKind::Primary);
        assert_eq!(out[0].confidence, Confidence::Unverified);
        assert_eq!(out[1].kind, SourceKind::Awesome);
        assert_eq!(out[1].confidence, Confidence::Unverified);
    }

    #[test]
    fn sanitize_drops_undrivable_and_dedupes() {
        let mut bad_url = source("bad", SourceKind::Primary, Confidence::Unverified);
        bad_url.url = "ftp://nope".to_string();
        let empty_id = source("  ", SourceKind::Primary, Confidence::Unverified);
        let dup = source("my-mirror", SourceKind::Primary, Confidence::Unverified);
        let out = sanitize_plugin_sources(vec![
            bad_url,
            empty_id,
            source("my-mirror", SourceKind::Primary, Confidence::Unverified),
            dup,
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "my-mirror");
        assert_eq!(out[0].order, 0);
    }

    #[test]
    fn sanitize_keeps_builtin_defaults_intact() {
        let out = sanitize_plugin_sources(default_plugin_sources());
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].id, "dsh-plugins");
        assert_eq!(out[0].confidence, Confidence::Official);
        assert_eq!(out[3].id, "github-topic");
        assert!(!out[3].enabled);
    }
}
