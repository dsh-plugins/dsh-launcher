// Plugin marketplace: fetches the plugin catalog from the market API and
// exposes per-channel versions (stable = releases/latest, beta =
// pre-releases/next, alpha = latest commit) plus install/enable plumbing.

use crate::config::{new_id, DshInstance};
use crate::AppState;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

/// Market API endpoint (dsh-plugins.github.io publishes to the custom domain).
const MARKET_URL: &str = "https://dsh-plug.in/api/plugins.json";
/// The community catalog (awesome-dsh-plugin.com), a different schema keyed by
/// an `install` command line; see `parse_awesome_install`.
const AWESOME_URL: &str = "https://awesome-dsh-plugin.com/plugins.json";
const NPM_REGISTRY: &str = "https://registry.npmjs.org";

/// Public OAuth App client id used to boost unauthenticated GitHub API quota
/// from 60 to 5000 requests/hour (an anonymous client-id parameter, no
/// authorization or token storage required). App: "DSH Launcher".
const GITHUB_CLIENT_ID: &str = "Ov23li6vtlVd83282YL6";

/// Build a GitHub API URL with the anonymous client-id quota boost.
/// `pub(crate)` so `update.rs` (launcher self-update check) can reuse the
/// same quota-boosted endpoint instead of the rate-limited `releases.atom`.
pub(crate) fn github_api_url(path: &str) -> String {
    let sep = if path.contains('?') { '&' } else { '?' };
    format!("https://api.github.com{path}{sep}client_id={GITHUB_CLIENT_ID}")
}

// ---------------------------------------------------------------------------
// Market catalog
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarketPluginDescription {
    pub language: String,
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarketPluginUrls {
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    #[serde(default)]
    pub issues: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarketPluginRelationship {
    /// The market JSON uses `type`; we expose it to the frontend as `kind`.
    #[serde(alias = "type")]
    pub kind: String,
    pub id: String,
    pub versions: String,
}

/// description can be a plain string or a localized list; normalise both.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MarketDescription {
    Plain(String),
    Localized(Vec<MarketPluginDescription>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarketPlugin {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<MarketDescription>,
    #[serde(default)]
    pub support_versions: Option<serde_json::Value>,
    #[serde(default)]
    pub urls: Option<MarketPluginUrls>,
    #[serde(default)]
    pub relationship: Option<Vec<MarketPluginRelationship>>,
    /// Which catalog this entry came from. Defaults to the primary dsh-plug.in
    /// catalog so old cached/frontend payloads without the field stay valid.
    #[serde(default)]
    pub source: PluginSource,
    /// Community-catalog extras (absent for the primary catalog).
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub stars: Option<u64>,
    #[serde(default)]
    pub downloads: Option<u64>,
}

/// Which catalog a market entry came from. Serialised lowercase so the
/// frontend filter can match on the string.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PluginSource {
    /// The primary catalog at dsh-plug.in (official, listed first).
    #[default]
    DshPlugins,
    /// The community catalog at awesome-dsh-plugin.com.
    AwesomeDshPlugin,
}

// ---------------------------------------------------------------------------
// Community catalog (awesome-dsh-plugin.com)
// ---------------------------------------------------------------------------

/// One entry in the awesome-dsh-plugin catalog. Only the fields the launcher
/// consumes are modelled; the rest (page/url/added/…) are ignored.
#[derive(Clone, Debug, Deserialize)]
struct AwesomePlugin {
    name: String,
    #[serde(default)]
    url: Option<String>,
    /// Bilingual description { en, zh }.
    #[serde(default)]
    description: Option<AwesomeDescription>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    stars: Option<u64>,
    #[serde(default)]
    downloads: Option<u64>,
    /// The install command line, e.g.
    /// `dsh plugin --profile web add @scope/pkg` (npm) or
    /// `dsh plugin --profile web add github:owner/repo` (GitHub).
    install: String,
}

#[derive(Clone, Debug, Deserialize)]
struct AwesomeDescription {
    #[serde(default)]
    en: Option<String>,
    #[serde(default)]
    zh: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct AwesomeCatalog {
    #[serde(default)]
    plugins: Vec<AwesomePlugin>,
}

/// Parses the `install` command line of an awesome-dsh-plugin entry into the
/// launcher's plugin id: an npm package spec (`@scope/pkg`), a GitHub spec
/// (`github:owner/repo`, optionally with `#path:<subdir>` for a plugin living
/// in a monorepo subdirectory), or a tarball URL (`tgz:https://…x.tgz`).
/// Returns None for anything we cannot drive.
///
/// The recognised shape is `dsh plugin --profile <name> add <target>` with
/// arbitrary flags tolerated; `<target>` is taken verbatim (surrounding
/// quotes stripped), so a trailing `@version` on an npm target is kept (the
/// version resolver splits it later).
fn parse_awesome_install(install: &str) -> Option<String> {
    let tokens: Vec<&str> = install.split_whitespace().collect();
    // Find the `add` subcommand; the install target is the next token.
    let pos = tokens.iter().position(|t| *t == "add")?;
    let target = tokens
        .get(pos + 1)?
        .trim()
        .trim_matches('"')
        .trim_matches('\'');
    if target.is_empty() {
        return None;
    }
    if let Some(rest) = target.strip_prefix("github:") {
        let (repo, subpath) = parse_github_body(rest)?;
        return Some(match subpath {
            Some(p) => format!("github:{repo}#path:{p}"),
            None => format!("github:{repo}"),
        });
    }
    // URL tarball (e.g. a GitHub release asset): pnpm installs it verbatim,
    // but it has no registry/channel metadata — the id is `tgz:<url>`.
    if (target.starts_with("https://") || target.starts_with("http://"))
        && (target.ends_with(".tgz") || target.ends_with(".tar.gz"))
    {
        return Some(format!("tgz:{target}"));
    }
    // npm target: a bare or scoped package name, optionally @version.
    // Reject anything with a scheme/host (not a plain registry spec).
    if target.contains("://") || target.contains(' ') {
        return None;
    }
    Some(target.to_string())
}

/// Splits the body of a `github:` spec (`owner/repo`, optionally followed by
/// `#path:<subdir>`) into its repo and subdirectory parts. Returns None when
/// the repo part is not exactly `owner/repo`, or when the fragment is
/// anything other than a `path:` — a committish is install-time state, not
/// part of the plugin's identity.
fn parse_github_body(body: &str) -> Option<(String, Option<String>)> {
    let (repo, frag) = match body.split_once('#') {
        Some((r, f)) => (r, Some(f)),
        None => (body, None),
    };
    let repo = repo.trim_end_matches(".git").trim_end_matches('/');
    // Must be owner/repo with both parts present.
    let mut parts = repo.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(o), Some(r), None) if !o.is_empty() && !r.is_empty() => {}
        _ => return None,
    }
    let subpath = match frag {
        None => None,
        Some(f) => {
            let p = f.strip_prefix("path:")?.trim_matches('/');
            if p.is_empty() {
                return None;
            }
            Some(p.to_string())
        }
    };
    Some((repo.to_string(), subpath))
}

/// Parses a launcher plugin id of the form `github:owner/repo` or
/// `github:owner/repo#path:<subdir>` into (repo, subdir).
pub(crate) fn parse_github_id(id: &str) -> Option<(String, Option<String>)> {
    parse_github_body(id.strip_prefix("github:")?)
}

/// Builds the pnpm install spec for a git-hosted plugin: the repo at `git_ref`
/// (a commit sha for alpha, a release tag for stable/beta), plus
/// `&path:<subdir>` for monorepo plugins (pnpm splits the fragment on '&').
pub(crate) fn github_install_spec(repo: &str, git_ref: &str, subpath: Option<&str>) -> String {
    match subpath {
        Some(p) => format!("github:{repo}#{git_ref}&path:{p}"),
        None => format!("github:{repo}#{git_ref}"),
    }
}

/// Converts one awesome-dsh-plugin entry into a `MarketPlugin`, or None when
/// its `install` line cannot be resolved to a drivable plugin id.
fn awesome_to_market(p: &AwesomePlugin) -> Option<MarketPlugin> {
    let id = parse_awesome_install(&p.install)?;
    let description = p.description.as_ref().and_then(|d| {
        let mut list = Vec::new();
        if let Some(en) = &d.en {
            list.push(MarketPluginDescription {
                language: "en".to_string(),
                content: en.clone(),
            });
        }
        if let Some(zh) = &d.zh {
            list.push(MarketPluginDescription {
                language: "zh".to_string(),
                content: zh.clone(),
            });
        }
        if list.is_empty() {
            None
        } else {
            Some(MarketDescription::Localized(list))
        }
    });
    let urls = p.url.as_ref().map(|u| MarketPluginUrls {
        homepage: None,
        repository: Some(u.clone()),
        issues: None,
    });
    Some(MarketPlugin {
        id,
        name: p.name.clone(),
        description,
        support_versions: None,
        urls,
        relationship: None,
        source: PluginSource::AwesomeDshPlugin,
        category: p.category.clone(),
        stars: p.stars,
        downloads: p.downloads,
    })
}

// ---------------------------------------------------------------------------
// Version channels
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PluginChannel {
    /// Releases / npm latest.
    Stable,
    /// Pre-releases / npm next.
    Beta,
    /// Latest commit on the default branch.
    Alpha,
}

#[derive(Clone, Debug, Serialize)]
pub struct PluginVersionInfo {
    /// The raw version identifier passed to the installer: a semver version
    /// for stable/beta, a commit hash for alpha.
    pub version: String,
    pub channel: PluginChannel,
    /// Short human label (e.g. the commit date or release tag).
    pub label: Option<String>,
    /// ISO publish/commit time; used by the UI to sort the mixed list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    /// Whether this is the channel's default (latest) entry.
    pub is_default: bool,
}

/// A page of versions. `has_more` is true when pagination can continue
/// (used by the alpha / commit channel).
#[derive(Clone, Debug, Serialize)]
pub struct PluginVersionPage {
    pub versions: Vec<PluginVersionInfo>,
    pub has_more: bool,
}

// ---------------------------------------------------------------------------
// Installed plugin (per instance/profile)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct InstalledPlugin {
    /// Package name / id (e.g. "@dsh-plugin/dsh-auxiliary").
    pub id: String,
    /// Installed version spec as recorded in the profile manifest.
    pub version: Option<String>,
    /// Whether the plugin is currently enabled (not disabled in cordis.patch.yml).
    pub enabled: bool,
    /// The cordis plugin id used in cordis.patch.yml (disables/insert rows).
    pub cordis_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallPluginInput {
    pub plugin_id: String,
    pub version: String,
    pub channel: PluginChannel,
    pub instance_id: String,
    pub profile: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPluginsEnabledInput {
    pub instance_id: String,
    pub profile: String,
    pub plugin_ids: Vec<String>,
    pub enabled: bool,
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

fn http_client() -> Result<reqwest::Client, String> {
    crate::proxy::apply(reqwest::Client::builder())
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("dsh-launcher")
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

/// Fetch and parse a JSON document with a size cap.
async fn fetch_json(url: &str, cap: usize) -> Result<serde_json::Value, String> {
    let client = http_client()?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("请求失败 {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("请求失败 {url}: HTTP {}", resp.status()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取响应失败 {url}: {e}"))?;
    if bytes.len() > cap {
        return Err(format!("响应过大 {url}"));
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("解析 JSON 失败 {url}: {e}"))
}

/// `pub(crate)` so `commands.rs` (GitHub release tag listing) can reuse the
/// same HTTP client and size cap.
pub(crate) async fn fetch_json_pub(url: &str, cap: usize) -> Result<serde_json::Value, String> {
    fetch_json(url, cap).await
}

// ---------------------------------------------------------------------------
// Commands: catalog
// ---------------------------------------------------------------------------

/// Fetches the marketplace plugin catalog from every configured source and
/// merges them. The primary dsh-plug.in catalog is listed first; entries from
/// the community catalog (awesome-dsh-plugin.com) follow. A source that fails
/// to fetch or parse is skipped (logged), never aborts the whole listing, and
/// a duplicate plugin id keeps the higher-priority (earlier) source's entry.
/// `query` filters by id/name/description (case-insensitive substring).
#[tauri::command(rename_all = "snake_case")]
pub async fn fetch_plugin_market(query: Option<String>) -> Result<Vec<MarketPlugin>, String> {
    // Fetch both catalogs concurrently; each returns Err on failure.
    let (primary_res, awesome_res) = tokio::join!(
        fetch_json(MARKET_URL, 4 * 1024 * 1024),
        fetch_json(AWESOME_URL, 8 * 1024 * 1024)
    );

    let mut plugins: Vec<MarketPlugin> = Vec::new();

    // Primary catalog (dsh-plug.in) first.
    match primary_res.and_then(|v| {
        serde_json::from_value::<Vec<MarketPlugin>>(v)
            .map_err(|e| format!("解析插件市场数据失败: {e}"))
    }) {
        Ok(mut list) => {
            for p in &mut list {
                p.source = PluginSource::DshPlugins;
            }
            plugins.extend(list);
        }
        Err(e) => crate::log_warn!("主插件源获取失败，忽略: {e}"),
    }

    // Community catalog (awesome-dsh-plugin.com) after; skip duplicate ids.
    match awesome_res.and_then(|v| {
        serde_json::from_value::<AwesomeCatalog>(v)
            .map_err(|e| format!("解析 awesome-dsh-plugin 数据失败: {e}"))
    }) {
        Ok(cat) => {
            for aw in &cat.plugins {
                let Some(mp) = awesome_to_market(aw) else {
                    crate::log_warn!(
                        "awesome 插件「{}」install 行无法解析，跳过: {}",
                        aw.name,
                        aw.install
                    );
                    continue;
                };
                if plugins.iter().any(|p| p.id == mp.id) {
                    crate::log_warn!("awesome 插件「{}」与主源 id 冲突，保留主源条目", mp.id);
                    continue;
                }
                plugins.push(mp);
            }
        }
        Err(e) => crate::log_warn!("awesome-dsh-plugin 插件源获取失败，忽略: {e}"),
    }

    let q = query
        .as_deref()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    let Some(q) = q else {
        return Ok(plugins);
    };

    let filtered = plugins
        .into_iter()
        .filter(|p| {
            if p.id.to_lowercase().contains(&q) || p.name.to_lowercase().contains(&q) {
                return true;
            }
            match &p.description {
                Some(MarketDescription::Plain(s)) => s.to_lowercase().contains(&q),
                Some(MarketDescription::Localized(list)) => {
                    list.iter().any(|d| d.content.to_lowercase().contains(&q))
                }
                None => false,
            }
        })
        .collect();
    Ok(filtered)
}

// ---------------------------------------------------------------------------
// Commands: versions per channel
// ---------------------------------------------------------------------------

/// Resolve the GitHub "owner/repo" for a plugin id. npm ids look up the
/// package's repository URL when urls.repository is absent; github: ids are
/// parsed directly.
fn github_repo_of(plugin: &MarketPlugin) -> Option<String> {
    if let Some(repo) = plugin
        .urls
        .as_ref()
        .and_then(|u| u.repository.as_ref().or(u.homepage.as_ref()))
    {
        if let Some(pos) = repo.find("github.com/") {
            let tail = &repo[pos + "github.com/".len()..];
            let tail = tail.trim_end_matches(".git").trim_end_matches('/');
            let mut parts = tail.split('/');
            if let (Some(owner), Some(name)) = (parts.next(), parts.next()) {
                if !owner.is_empty() && !name.is_empty() {
                    return Some(format!("{owner}/{name}"));
                }
            }
        }
    }
    if let Some((repo, _subpath)) = parse_github_id(&plugin.id) {
        return Some(repo);
    }
    None
}

/// Fetches versions for a plugin across the requested channel.
/// - stable/beta read the npm registry dist-tags (latest / next) and fall
///   back to the version list ordered by publish time (all at once).
/// - alpha pages through the GitHub commit history (30 per page); `page` is
///   1-based and defaults to 1. `has_more` tells the UI to lazy-load more.
#[tauri::command(rename_all = "snake_case")]
pub async fn fetch_plugin_versions(
    plugin_id: String,
    channel: PluginChannel,
    page: Option<u32>,
) -> Result<PluginVersionPage, String> {
    // URL tarballs have no registry/channels: a single pseudo-version on
    // stable; other channels are empty.
    if plugin_id.starts_with("tgz:") {
        let versions = match channel {
            PluginChannel::Stable => vec![PluginVersionInfo {
                version: "latest".to_string(),
                channel: channel.clone(),
                label: Some(plugin_id.clone()),
                published_at: None,
                is_default: true,
            }],
            _ => Vec::new(),
        };
        return Ok(PluginVersionPage {
            versions,
            has_more: false,
        });
    }
    match channel {
        PluginChannel::Stable | PluginChannel::Beta => {
            // Git-hosted plugins have no npm registry entry; their release
            // channels come from the repo's GitHub releases instead (stable
            // = full releases, beta = prereleases).
            if plugin_id.starts_with("github:") {
                return github_release_versions(&plugin_id, &channel).await;
            }
            let versions = npm_versions(&plugin_id, &channel).await?;
            Ok(PluginVersionPage {
                versions,
                has_more: false,
            })
        }
        PluginChannel::Alpha => alpha_commit(&plugin_id, page.unwrap_or(1)).await,
    }
}

/// Fetches GitHub releases as versions for a `github:` plugin, newest first:
/// stable keeps full releases, beta keeps prereleases. The release's tag name
/// becomes the install ref; the channel's newest entry is the default.
async fn github_release_versions(
    plugin_id: &str,
    channel: &PluginChannel,
) -> Result<PluginVersionPage, String> {
    let (repo, _subpath) = parse_github_id(plugin_id)
        .ok_or_else(|| format!("无法解析 GitHub 插件 id: {plugin_id}"))?;
    let url = github_api_url(&format!("/repos/{repo}/releases?per_page=30"));
    let doc = fetch_json(&url, 4 * 1024 * 1024).await?;
    let want_prerelease = matches!(channel, PluginChannel::Beta);
    let mut out: Vec<PluginVersionInfo> = Vec::new();
    if let Some(arr) = doc.as_array() {
        for rel in arr {
            if rel.get("draft").and_then(|v| v.as_bool()).unwrap_or(false) {
                continue;
            }
            let prerelease = rel
                .get("prerelease")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if prerelease != want_prerelease {
                continue;
            }
            let tag = rel
                .get("tag_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if tag.is_empty() {
                continue;
            }
            let name = rel
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let date = rel.get("published_at").and_then(|v| v.as_str());
            let label = match (name, date) {
                (Some(n), Some(d)) => Some(format!("{d} · {n}")),
                (Some(n), None) => Some(n.to_string()),
                (None, Some(d)) => Some(d.to_string()),
                _ => None,
            };
            out.push(PluginVersionInfo {
                version: tag,
                channel: channel.clone(),
                label,
                published_at: date.map(|d| d.to_string()),
                is_default: false,
            });
        }
    }
    if let Some(first) = out.first_mut() {
        first.is_default = true;
    }
    Ok(PluginVersionPage {
        versions: out,
        has_more: false,
    })
}

async fn npm_versions(
    plugin_id: &str,
    channel: &PluginChannel,
) -> Result<Vec<PluginVersionInfo>, String> {
    // Scoped npm packages must be URL-encoded (@dsh-plugin/x -> @dsh-plugin%2fx).
    let encoded = plugin_id.replace('/', "%2f");
    let url = format!("{NPM_REGISTRY}/{encoded}");
    let doc = fetch_json(&url, 16 * 1024 * 1024).await?;

    let dist_tags = doc
        .get("dist-tags")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let tag_name = match channel {
        PluginChannel::Stable => "latest",
        PluginChannel::Beta => "next",
        PluginChannel::Alpha => unreachable!(),
    };

    // The channel's default (dist-tag) version, if present.
    let default_version = dist_tags
        .get(tag_name)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Collect all published versions with their release time, newest first.
    let time = doc.get("time").cloned().unwrap_or(serde_json::json!({}));
    let mut versions: Vec<(String, String)> = Vec::new();
    if let Some(obj) = time.as_object() {
        for (ver, ts) in obj {
            if ver == "created" || ver == "modified" {
                continue;
            }
            let ts = ts.as_str().unwrap_or("").to_string();
            versions.push((ver.clone(), ts));
        }
    }
    versions.sort_by(|a, b| b.1.cmp(&a.1)); // newest first by ISO time

    // Publish time of the dist-tag default, for the synthetic fallback entry.
    let default_ts = default_version.as_deref().and_then(|def| {
        versions
            .iter()
            .find(|(v, _)| v == def)
            .map(|(_, ts)| ts.clone())
            .filter(|ts| !ts.is_empty())
    });

    // Filter per channel: stable = no pre-release tag, beta = pre-release tag.
    let is_prerelease = |v: &str| v.contains('-');
    let mut out: Vec<PluginVersionInfo> = Vec::new();
    for (ver, ts) in versions {
        let include = match channel {
            PluginChannel::Stable => !is_prerelease(&ver),
            PluginChannel::Beta => is_prerelease(&ver),
            PluginChannel::Alpha => unreachable!(),
        };
        if !include {
            continue;
        }
        let is_default = default_version.as_deref() == Some(ver.as_str());
        out.push(PluginVersionInfo {
            version: ver,
            channel: channel.clone(),
            label: if ts.is_empty() {
                None
            } else {
                Some(ts.clone())
            },
            published_at: if ts.is_empty() { None } else { Some(ts) },
            is_default,
        });
    }

    // Make sure the dist-tag default is present even if it didn't pass the
    // filter (e.g. a `latest` that is itself a pre-release).
    if let Some(def) = default_version {
        if !out.iter().any(|v| v.version == def) {
            out.insert(
                0,
                PluginVersionInfo {
                    version: def,
                    channel: channel.clone(),
                    label: Some("dist-tag".to_string()),
                    published_at: default_ts.clone(),
                    is_default: true,
                },
            );
        }
    }
    Ok(out)
}

/// Fetches one page of the commit history (alpha channel). GitHub commits
/// API returns up to `per_page` items; `has_more` is true when a full page
/// came back. `is_default` marks the first commit of page 1.
async fn alpha_commit(plugin_id: &str, page: u32) -> Result<PluginVersionPage, String> {
    // Alpha needs the GitHub repo; it is derived from the market entry.
    let catalog = fetch_plugin_market(None).await?;
    let plugin = catalog
        .iter()
        .find(|p| p.id == plugin_id)
        .ok_or_else(|| format!("插件 {plugin_id} 不在市场中"))?;
    let repo = github_repo_of(plugin)
        .ok_or_else(|| format!("插件 {plugin_id} 没有可用的 GitHub 仓库地址"))?;

    // Monorepo plugins (`github:owner/repo#path:<subdir>`): restrict the
    // commit list to commits touching the plugin's own directory.
    let path_filter = parse_github_id(plugin_id)
        .and_then(|(_, subpath)| subpath)
        .map(|p| format!("&path={p}"))
        .unwrap_or_default();

    const PER_PAGE: u32 = 30;
    let url = github_api_url(&format!(
        "/repos/{repo}/commits?per_page={PER_PAGE}&page={page}{path_filter}"
    ));
    let doc = fetch_json(&url, 4 * 1024 * 1024).await?;
    let mut out: Vec<PluginVersionInfo> = Vec::new();
    if let Some(arr) = doc.as_array() {
        for (i, commit) in arr.iter().enumerate() {
            let sha = commit
                .get("sha")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if sha.is_empty() {
                continue;
            }
            let message = commit
                .pointer("/commit/message")
                .and_then(|v| v.as_str())
                .map(|s| s.lines().next().unwrap_or("").to_string());
            let date = commit
                .pointer("/commit/committer/date")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let label = match (&message, &date) {
                (Some(m), Some(d)) => Some(format!("{d} · {m}")),
                (Some(m), None) => Some(m.clone()),
                (None, Some(d)) => Some(d.clone()),
                _ => None,
            };
            out.push(PluginVersionInfo {
                version: sha,
                channel: PluginChannel::Alpha,
                label,
                published_at: date,
                is_default: page == 1 && i == 0,
            });
        }
    }
    let has_more = out.len() as u32 == PER_PAGE;
    Ok(PluginVersionPage {
        versions: out,
        has_more,
    })
}

// ---------------------------------------------------------------------------
// Profile manifest helpers (read/write package.json + cordis.patch.yml)
// ---------------------------------------------------------------------------

/// Path of a profile dir under a DSH_HOME.
fn profile_dir(home_path: &std::path::Path, profile: &str) -> std::path::PathBuf {
    home_path.join("profiles").join(profile)
}

/// `pub(crate)` for the modpack module (issue #5).
pub(crate) fn profile_dir_pub(home_path: &std::path::Path, profile: &str) -> std::path::PathBuf {
    profile_dir(home_path, profile)
}

/// Read the profile package.json (dsh.profile.bundles + dependencies).
fn read_profile_manifest(dir: &std::path::Path) -> Result<serde_json::Value, String> {
    let path = dir.join("package.json");
    if !path.exists() {
        return Ok(serde_json::json!({
            "private": true,
            "dependencies": {},
            "dsh": { "profile": { "bundles": [] } },
        }));
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("读取 package.json 失败: {e}"))?;
    serde_json::from_str(&raw).map_err(|e| format!("解析 package.json 失败: {e}"))
}

/// cordis id for a package: bundles register under their unscoped short name
/// (dsh-auxiliary) unless the package declares otherwise. We default to the
/// last path segment without the scope.
pub fn cordis_id_of(package: &str) -> String {
    let last = package.rsplit('/').next().unwrap_or(package);
    last.to_string()
}

// ---------------------------------------------------------------------------
// Commands: installed plugin listing (per instance + profile)
// ---------------------------------------------------------------------------

/// Lists plugins installed into an instance's profile, excluding core
/// @deepseek-ai/* packages. Reads the profile manifest (dependencies +
/// bundles) and cordis.patch.yml (disabled rows).
#[tauri::command(rename_all = "snake_case")]
pub async fn list_installed_plugins(
    state: State<'_, AppState>,
    instance_id: String,
    profile: String,
) -> Result<Vec<InstalledPlugin>, String> {
    let (home_path, _version) = resolve_instance(&state, &instance_id)?;
    let dir = profile_dir(&home_path, &profile);
    let manifest = read_profile_manifest(&dir)?;

    let mut ids: Vec<String> = Vec::new();
    let mut versions: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    if let Some(deps) = manifest.get("dependencies").and_then(|d| d.as_object()) {
        for (name, spec) in deps {
            if name.starts_with("@deepseek-ai/") {
                continue;
            }
            ids.push(name.clone());
            versions.insert(name.clone(), spec.as_str().unwrap_or("").to_string());
        }
    }
    if let Some(bundles) = manifest
        .pointer("/dsh/profile/bundles")
        .and_then(|b| b.as_array())
    {
        for b in bundles {
            if let Some(name) = b.as_str() {
                if name.starts_with("@deepseek-ai/") || ids.iter().any(|i| i == name) {
                    continue;
                }
                ids.push(name.to_string());
            }
        }
    }
    ids.sort();
    ids.dedup();

    // Disabled set from cordis.patch.yml (`- id: <cordis-id>` + `disabled: true`).
    let disabled = read_disabled_ids(&dir);

    let out = ids
        .into_iter()
        .map(|id| {
            let cordis_id = cordis_id_of(&id);
            let enabled = !disabled.contains(&cordis_id) && !disabled.contains(&id);
            InstalledPlugin {
                version: versions.get(&id).cloned(),
                enabled,
                cordis_id: Some(cordis_id),
                id,
            }
        })
        .collect();
    Ok(out)
}

/// Parse disabled cordis ids from a profile's cordis.patch.yml. We do a
/// lightweight line scan (avoid pulling a YAML parser dependency for this).
fn read_disabled_ids(dir: &std::path::Path) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    let path = dir.join("cordis.patch.yml");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return set;
    };
    let mut current_id: Option<String> = None;
    for line in raw.lines() {
        let t = line.trim();
        if t.starts_with("- id:") {
            current_id = Some(t.trim_start_matches("- id:").trim().to_string());
        } else if t.starts_with("id:") && !line.starts_with(' ') && !line.starts_with('\t') {
            current_id = Some(t.trim_start_matches("id:").trim().to_string());
        } else if t == "disabled: true" {
            if let Some(id) = current_id.take() {
                set.insert(id);
            }
        } else if t.starts_with("- ") && !t.starts_with("- id:") {
            current_id = None;
        }
    }
    set
}

/// Resolve an instance to (home_path, version_dir).
pub(crate) fn resolve_instance(
    state: &State<'_, AppState>,
    instance_id: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let cfg = state.config.lock().unwrap();
    let inst: &DshInstance = cfg
        .instances
        .iter()
        .find(|i| i.id == instance_id)
        .ok_or_else(|| "实例不存在".to_string())?;
    let home = cfg
        .homes
        .iter()
        .find(|h| h.id == inst.home_id)
        .ok_or_else(|| "DSH_HOME 不存在".to_string())?;
    let version = cfg
        .versions
        .iter()
        .find(|v| v.id == inst.version_id)
        .ok_or_else(|| "版本不存在".to_string())?;
    Ok((home.path.clone(), version.dir.clone()))
}

// ---------------------------------------------------------------------------
// Commands: enable / disable (cordis.patch.yml disabled rows)
// ---------------------------------------------------------------------------

/// Sets plugins enabled/disabled in a profile's cordis.patch.yml by adding or
/// removing `disabled: true` rows. Batch-capable via plugin_ids.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_plugins_enabled(
    state: State<'_, AppState>,
    input: SetPluginsEnabledInput,
) -> Result<(), String> {
    let (home_path, _) = resolve_instance(&state, &input.instance_id)?;
    let dir = profile_dir(&home_path, &input.profile);
    let patch_path = dir.join("cordis.patch.yml");

    let mut raw = if patch_path.exists() {
        std::fs::read_to_string(&patch_path)
            .map_err(|e| format!("读取 cordis.patch.yml 失败: {e}"))?
    } else {
        String::new()
    };

    for package in &input.plugin_ids {
        let cordis_id = cordis_id_of(package);
        raw = set_disabled_row(&raw, &cordis_id, input.enabled);
    }

    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 profile 目录失败: {e}"))?;
    std::fs::write(&patch_path, raw).map_err(|e| format!("写入 cordis.patch.yml 失败: {e}"))?;
    Ok(())
}

/// Input for uninstalling a plugin from a profile.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UninstallPluginInput {
    pub instance_id: String,
    pub profile: String,
    pub plugin_id: String,
}

/// Uninstalls a plugin from an instance's profile through
/// `dsh plugin --profile <name> remove <id>` (the CLI removes the dependency
/// and reconciles dsh.profile.bundles), then drops the plugin's
/// cordis.patch.yml rows (insert / disabled), which the CLI does not manage.
#[tauri::command(rename_all = "snake_case")]
pub async fn uninstall_plugin(
    app: AppHandle,
    state: State<'_, AppState>,
    input: UninstallPluginInput,
) -> Result<(), String> {
    let (home_path, version_dir) = resolve_instance(&state, &input.instance_id)?;
    let dir = profile_dir(&home_path, &input.profile);
    if !dir.exists() {
        return Err(format!("Profile「{}」不存在", input.profile));
    }

    // 0. Same profile serialization as installs: a removal also rewrites the
    //    manifest, so it must not race an install into the same profile.
    let lock = profile_lock(&state, &dir).await;
    let _guard = lock.lock().await;

    // 1. `dsh plugin remove <id>` through the instance's own CLI: it removes
    //    the dependency and reconciles dsh.profile.bundles (a name that is no
    //    longer an installed bundle leaves the layer stack), so the manifest is
    //    never edited by hand here.
    run_dsh_plugin(
        &app,
        &state,
        "uninstall",
        &PluginCliTarget {
            version_dir: &version_dir,
            home_path: &home_path,
            profile: &input.profile,
        },
        &PluginCliOp {
            subcommand: "remove",
            spec: &input.plugin_id,
            loglevel: "warn",
        },
    )
    .await?;

    // 2. Drop the plugin's rows from cordis.patch.yml (insert rows mount the
    //    plugin; disabled rows gate it). Reuse the block-stripping logic in
    //    set_disabled_row by removing any block whose id matches.
    let patch_path = dir.join("cordis.patch.yml");
    if patch_path.exists() {
        let raw = std::fs::read_to_string(&patch_path)
            .map_err(|e| format!("读取 cordis.patch.yml 失败: {e}"))?;
        let cordis_id = cordis_id_of(&input.plugin_id);
        let cleaned = strip_cordis_rows(&raw, &cordis_id, &input.plugin_id);
        if cleaned != raw {
            std::fs::write(&patch_path, &cleaned)
                .map_err(|e| format!("写入 cordis.patch.yml 失败: {e}"))?;
        }
    }

    Ok(())
}

/// Strips every cordis.patch.yml block whose id equals `cordis_id` (matching
/// plain `- id:` / `id:` rows, including `- insert:` wrappers) and restores
/// the `[]` placeholder when the document becomes empty.
fn strip_cordis_rows(raw: &str, cordis_id: &str, plugin_id: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut skip = false;
    for line in raw.lines() {
        let t = line.trim();
        if t == "[]" {
            continue;
        }
        // Start of a block for the target: `- id: <id>` (plain or insert row).
        let is_target = t == format!("- id: {cordis_id}")
            || t == format!("id: {cordis_id}")
            || t == format!("- id: {plugin_id}")
            || t == format!("id: {plugin_id}");
        if is_target {
            skip = true;
            continue;
        }
        if skip {
            // Inside a target block: drop indented child lines and blank
            // separators; stop at the next top-level key.
            if t.is_empty() {
                continue;
            }
            let indent = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
            if indent > 0 {
                continue;
            }
            skip = false;
        }
        out.push(line.to_string());
    }

    let mut cleaned: Vec<String> = out;
    while cleaned.last().map(|l| l.trim().is_empty()) == Some(true) {
        cleaned.pop();
    }
    let mut result = cleaned.join("\n");
    if !result.ends_with('\n') {
        result.push('\n');
    }
    let body: String = result
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#')
        })
        .collect::<Vec<_>>()
        .join("\n");
    if body.trim().is_empty() {
        if !result.ends_with('\n') {
            result.push('\n');
        }
        result.push_str("[]\n");
    }
    result
}

/// Add or remove a `disabled: true` row for a cordis id in cordis.patch.yml.
fn set_disabled_row(raw: &str, cordis_id: &str, enabled: bool) -> String {
    // Remove any existing rows for this id (both plain and commented forms).
    let mut out: Vec<String> = Vec::new();
    let mut skip_block = false;
    for line in raw.lines() {
        let t = line.trim();
        // A top-level `[]` placeholder is dropped when we have any real entry
        // to write; it is kept only while the document stays empty.
        if t == "[]" {
            continue;
        }
        let is_target_id = t == format!("- id: {cordis_id}") || t == format!("id: {cordis_id}");
        if is_target_id {
            // Start of a block for this id; look ahead: if it is a pure
            // `disabled: true` block we drop it entirely.
            skip_block = true;
            continue;
        }
        if skip_block {
            // Inside the block: only `disabled:` and blank lines belong to it.
            if t == "disabled: true" || t == "disabled: false" || t.is_empty() {
                skip_block = false; // end of this small block
                continue;
            }
            // Block has other content (config etc.) — keep it, stop skipping.
            skip_block = false;
            out.push(line.to_string());
            continue;
        }
        out.push(line.to_string());
    }

    let mut cleaned: Vec<String> = out;
    // Trim trailing blank lines.
    while cleaned.last().map(|l| l.trim().is_empty()) == Some(true) {
        cleaned.pop();
    }

    if !enabled {
        // Append a fresh disable row (block sequence, never after `[]`).
        cleaned.push(String::new());
        cleaned.push(format!("- id: {cordis_id}"));
        cleaned.push("  disabled: true".to_string());
    }

    let mut result = cleaned.join("\n");
    if !result.ends_with('\n') {
        result.push('\n');
    }
    // If the document became empty again (everything removed), restore the
    // `[]` placeholder so the file stays a valid top-level array.
    let body: String = result
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#')
        })
        .collect::<Vec<_>>()
        .join("\n");
    if body.trim().is_empty() {
        if !result.ends_with('\n') {
            result.push('\n');
        }
        result.push_str("[]\n");
    }
    result
}

// ---------------------------------------------------------------------------
// Commands: install task
// ---------------------------------------------------------------------------

/// Enqueues an install task: pnpm add <pkg>@<version> into the profile dir,
/// register the bundle in package.json (dsh.profile.bundles) and cordis.patch
/// insert row for non-bundle plugins. Reuses the shared pnpm store and the
/// onlyBuiltDependencies build-script opt-in.
#[tauri::command(rename_all = "snake_case")]
pub async fn start_install_plugin_task(
    app: AppHandle,
    state: State<'_, AppState>,
    input: InstallPluginInput,
) -> Result<String, String> {
    // Validate instance + profile early.
    let (home_path, _version) = resolve_instance(&state, &input.instance_id)?;
    let dir = profile_dir(&home_path, &input.profile);
    if !dir.exists() {
        return Err(format!("Profile「{}」不存在", input.profile));
    }

    let (label, display_name) = match input.plugin_id.strip_prefix("tgz:") {
        Some(target) => {
            let base = target.rsplit(['/', '\\']).next().unwrap_or(target);
            (
                format!(
                    "安装插件包 {base} 到「{}」的 profile「{}」",
                    input.instance_id, input.profile
                ),
                base.to_string(),
            )
        }
        None => (
            format!(
                "安装插件 {}@{} 到「{}」的 profile「{}」",
                input.plugin_id, input.version, input.instance_id, input.profile
            ),
            input.plugin_id.clone(),
        ),
    };
    let task = crate::tasks::TaskInfo {
        id: new_id("t"),
        kind: "install-plugin".to_string(),
        label,
        version: input.version.clone(),
        state: crate::tasks::TaskState::Running,
        percent: 0,
        created_at: crate::tasks::now_millis_pub(),
        message: None,
        instance_id: Some(input.instance_id.clone()),
        instance_name: Some(display_name),
        reserved_home_path: None,
        logs: Vec::new(),
        child: None,
    };
    let task_id = task.id.clone();
    state.tasks.lock().await.insert(task_id.clone(), task);
    crate::tasks::emit_progress_pub(
        &app,
        &task_id,
        crate::tasks::TaskState::Running,
        0,
        None,
        None,
    );

    let worker_app = app.clone();
    let worker_task_id = task_id.clone();
    let input = input.clone();
    tauri::async_runtime::spawn(async move {
        let state = worker_app.state::<AppState>();
        run_install_plugin_task(&worker_app, &state, &worker_task_id, input).await;
    });

    Ok(task_id)
}

/// Installs a plugin from a local `.tgz` tarball (file picker or drag-drop).
/// The tarball is handed to the instance's CLI verbatim (`pnpm add` accepts
/// local tarballs); the recorded dependency name is resolved back from the
/// profile manifest afterwards, same as registry/git installs.
#[tauri::command(rename_all = "snake_case")]
pub async fn start_install_plugin_file_task(
    app: AppHandle,
    state: State<'_, AppState>,
    instance_id: String,
    profile: String,
    path: String,
) -> Result<String, String> {
    let lower = path.to_lowercase();
    if !(lower.ends_with(".tgz") || lower.ends_with(".tar.gz")) {
        return Err("仅支持 .tgz / .tar.gz 插件包".to_string());
    }
    if !std::path::Path::new(&path).is_file() {
        return Err(format!("插件包不存在: {path}"));
    }
    // pnpm treats Windows paths more reliably with forward slashes.
    let spec_path = path.replace('\\', "/");
    let input = InstallPluginInput {
        plugin_id: format!("tgz:{spec_path}"),
        version: "local".to_string(),
        channel: PluginChannel::Stable,
        instance_id,
        profile,
    };
    start_install_plugin_task(app, state, input).await
}

async fn run_install_plugin_task(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    input: InstallPluginInput,
) {
    let result = do_install_plugin(app, state, task_id, &input).await;
    let mut tasks = state.tasks.lock().await;
    if let Some(task) = tasks.get_mut(task_id) {
        if task.state == crate::tasks::TaskState::Cancelled {
            return;
        }
        match result {
            Ok(()) => {
                task.state = crate::tasks::TaskState::Done;
                task.percent = 100;
                crate::tasks::emit_progress_pub(
                    app,
                    task_id,
                    crate::tasks::TaskState::Done,
                    100,
                    None,
                    Some(input.instance_id.clone()),
                );
            }
            Err(msg) => {
                task.state = crate::tasks::TaskState::Error;
                task.message = Some(msg.clone());
                crate::tasks::push_log_locked_pub(task, &format!("error: {msg}"));
                let pct = task.percent;
                drop(tasks);
                crate::tasks::emit_progress_pub(
                    app,
                    task_id,
                    crate::tasks::TaskState::Error,
                    pct,
                    Some(msg),
                    None,
                );
            }
        }
    }
}

async fn do_install_plugin(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    input: &InstallPluginInput,
) -> Result<(), String> {
    let (home_path, version_dir) = resolve_instance(state, &input.instance_id)?;
    let dir = profile_dir(&home_path, &input.profile);

    // Spec: tarball ids (`tgz:`) install the URL/path verbatim; npm packages
    // use <pkg>@<version>; git-hosted (github:) plugins install the repo at a
    // ref — a commit sha for alpha, a release tag for stable/beta — plus an
    // optional `&path:<subdir>` for monorepo plugins.
    let spec = match input.plugin_id.strip_prefix("tgz:") {
        Some(target) => target.to_string(),
        None => match parse_github_id(&input.plugin_id) {
            Some((repo, subpath)) => github_install_spec(&repo, &input.version, subpath.as_deref()),
            None => match input.channel {
                PluginChannel::Alpha => {
                    let catalog = fetch_plugin_market(None).await?;
                    let plugin = catalog
                        .iter()
                        .find(|p| p.id == input.plugin_id)
                        .ok_or_else(|| format!("插件 {} 不在市场中", input.plugin_id))?;
                    let repo = github_repo_of(plugin)
                        .ok_or_else(|| format!("插件 {} 没有 GitHub 仓库", input.plugin_id))?;
                    format!("github:{repo}#{}", input.version)
                }
                _ => format!("{}@{}", input.plugin_id, input.version),
            },
        },
    };

    crate::tasks::push_task_log_pub(
        app,
        state,
        task_id,
        &format!("安装 {spec} 到 {}", dir.display()),
    )
    .await;

    // 0. Serialize against this profile: `dsh plugin` is a read-modify-write
    //    cycle over the profile's package.json (pnpm writes dependencies, then
    //    the CLI reconciles dsh.profile.bundles from the installed state), so
    //    parallel installs into one profile clobber each other and only the
    //    last plugin survives. Queue instead.
    let lock = profile_lock(state, &dir).await;
    let guard = match lock.try_lock() {
        Ok(g) => g,
        Err(_) => {
            // Queued behind another operation on this profile: reflect that in
            // the task state so the UI shows the queue depth instead of a row
            // of "running" tasks that are actually waiting.
            set_task_queued(app, state, task_id).await;
            crate::tasks::push_task_log_pub(
                app,
                state,
                task_id,
                "该 profile 正在执行其他插件操作，排队等待…",
            )
            .await;
            lock.lock().await
        }
    };
    let _guard = guard;
    // Cancelled while queued: stop before touching the profile.
    if task_cancelled(state, task_id).await {
        return Err("已取消".to_string());
    }
    set_task_running(app, state, task_id).await;

    // 1. `dsh plugin add <spec>` through the instance's own CLI: it installs
    //    into the profile dir and reconciles dsh.profile.bundles itself, so the
    //    launcher neither drives pnpm nor guesses the layer list.
    run_dsh_plugin(
        app,
        state,
        task_id,
        &PluginCliTarget {
            version_dir: &version_dir,
            home_path: &home_path,
            profile: &input.profile,
        },
        &PluginCliOp {
            subcommand: "add",
            spec: &spec,
            loglevel: "http",
        },
    )
    .await?;

    // 2. Resolve the real package name the plugin was recorded under. pnpm
    //    keys dependencies by package name: for npm installs that IS the
    //    plugin id, but for git-hosted installs the id is a `github:` spec
    //    and the spec lands in the dependency VALUE. Every downstream id use
    //    (bundles check, cordis mount row) needs the real name — mounting the
    //    raw github: spec makes cordis import it as an ESM URL and the whole
    //    profile fails to boot with ERR_UNSUPPORTED_ESM_URL_SCHEME.
    let installed_name = match resolve_installed_name(&dir, &input.plugin_id, &spec) {
        Some(name) => {
            if name != input.plugin_id {
                crate::tasks::push_task_log_pub(
                    app,
                    state,
                    task_id,
                    &format!("插件已安装为包「{name}」"),
                )
                .await;
            }
            name
        }
        None => {
            if input.plugin_id.starts_with("github:") || input.plugin_id.starts_with("tgz:") {
                // Boot safety: never mount an unresolved github:/tgz: id.
                let msg = format!(
                    "无法在 package.json 中定位 {} 的已安装包名，跳过 cordis 挂载（请检查 profile）",
                    input.plugin_id
                );
                crate::log_warn!("{msg}");
                crate::tasks::push_task_log_pub(app, state, task_id, &msg).await;
                return Ok(());
            }
            input.plugin_id.clone()
        }
    };

    // 3. Bundle registration is the CLI's job (reconciled from the installed
    //    state). Read back its verdict: a package listed in
    //    dsh.profile.bundles is mounted as a profile layer, and adding a
    //    cordis.patch.yml insert row on top would mount it twice — the loader
    //    then fails with `duplicate loader entry id`. Only a plain package
    //    (no `dsh.bundle.patch`, so not reconciled into bundles) needs the
    //    explicit insert row.
    if manifest_lists_bundle(&dir, &installed_name)? {
        crate::tasks::push_task_log_pub(
            app,
            state,
            task_id,
            "CLI 已将插件登记为 profile 层（dsh.profile.bundles），跳过 cordis insert 行",
        )
        .await;
    } else {
        ensure_cordis_insert(&dir, &installed_name)?;
        crate::tasks::push_task_log_pub(
            app,
            state,
            task_id,
            "插件未声明 dsh.bundle.patch，已写入 cordis.patch.yml insert 行以挂载",
        )
        .await;
    }

    Ok(())
}

/// Whether the profile manifest lists the package in `dsh.profile.bundles`
/// after the CLI reconciled it.
fn manifest_lists_bundle(profile_dir: &std::path::Path, plugin_id: &str) -> Result<bool, String> {
    let manifest = read_profile_manifest(profile_dir)?;
    Ok(manifest
        .pointer("/dsh/profile/bundles")
        .and_then(|b| b.as_array())
        .map(|arr| arr.iter().any(|b| b.as_str() == Some(plugin_id)))
        .unwrap_or(false))
}

/// Resolves the dependency key under which an install landed in the profile
/// manifest: the real package name of the installed plugin. npm installs key
/// by package name already; git-hosted installs record the `github:` spec as
/// the dependency VALUE under the real name. Returns None when nothing in
/// `dependencies` matches.
fn resolve_installed_name(dir: &std::path::Path, plugin_id: &str, spec: &str) -> Option<String> {
    let manifest = read_profile_manifest(dir).ok()?;
    let deps = manifest.get("dependencies")?.as_object()?;
    // pnpm records a git-hosted spec verbatim as the dependency value.
    for (name, value) in deps {
        if value.as_str() == Some(spec) {
            return Some(name.clone());
        }
    }
    // Tarball installs may be recorded with a normalized value (file:…,
    // resolved mirrors); fall back to matching the tarball's basename.
    if let Some(target) = plugin_id.strip_prefix("tgz:") {
        let base = target.rsplit(['/', '\\']).next().unwrap_or(target);
        for (name, value) in deps {
            if value.as_str().map(|v| v.ends_with(base)).unwrap_or(false) {
                return Some(name.clone());
            }
        }
        return None;
    }
    // npm install: the key is the package name (the spec may carry @version).
    let base = match plugin_id.rfind('@') {
        Some(i) if i > 0 => &plugin_id[..i],
        _ => plugin_id,
    };
    if deps.contains_key(base) {
        return Some(base.to_string());
    }
    // The CLI/pnpm may normalise the recorded value; accept any value that
    // starts with the github: repo part of the spec.
    if let Some((repo, _)) = spec.split('#').next().and_then(parse_github_id) {
        let prefix = format!("github:{repo}");
        for (name, value) in deps {
            if value.as_str().is_some_and(|v| v.starts_with(&prefix)) {
                return Some(name.clone());
            }
        }
    }
    None
}

/// Lock key for a profile directory. Windows filesystems are
/// case-insensitive, so the key is lowercased there: two instances that share
/// a DSH_HOME must contend for the same lock.
fn profile_lock_key(profile_dir: &std::path::Path) -> String {
    let raw = profile_dir.to_string_lossy().to_string();
    if cfg!(windows) {
        raw.to_lowercase()
    } else {
        raw
    }
}

/// The mutex guarding one profile directory, created on first use.
async fn profile_lock(
    state: &State<'_, AppState>,
    profile_dir: &std::path::Path,
) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    let key = profile_lock_key(profile_dir);
    let mut locks = state.profile_locks.lock().await;
    locks.entry(key).or_default().clone()
}

/// Marks the task as waiting in the profile queue (no work started yet).
async fn set_task_queued(app: &AppHandle, state: &State<'_, AppState>, task_id: &str) {
    let mut tasks = state.tasks.lock().await;
    if let Some(task) = tasks.get_mut(task_id) {
        if task.state == crate::tasks::TaskState::Cancelled {
            return;
        }
        task.state = crate::tasks::TaskState::Queued;
        task.percent = 0;
    }
    drop(tasks);
    crate::tasks::emit_progress_pub(app, task_id, crate::tasks::TaskState::Queued, 0, None, None);
}

/// Promotes a queued task to running once it owns the profile lock.
async fn set_task_running(app: &AppHandle, state: &State<'_, AppState>, task_id: &str) {
    let mut tasks = state.tasks.lock().await;
    if let Some(task) = tasks.get_mut(task_id) {
        if task.state == crate::tasks::TaskState::Cancelled {
            return;
        }
        task.state = crate::tasks::TaskState::Running;
    }
    drop(tasks);
    crate::tasks::emit_progress_pub(
        app,
        task_id,
        crate::tasks::TaskState::Running,
        0,
        None,
        None,
    );
}

/// Whether the task was cancelled while it waited in the queue.
async fn task_cancelled(state: &State<'_, AppState>, task_id: &str) -> bool {
    state
        .tasks
        .lock()
        .await
        .get(task_id)
        .map(|t| t.state == crate::tasks::TaskState::Cancelled)
        .unwrap_or(false)
}

/// Which instance/profile a `dsh plugin` invocation targets.
struct PluginCliTarget<'a> {
    version_dir: &'a std::path::Path,
    home_path: &'a std::path::Path,
    profile: &'a str,
}

/// What the invocation does: a pnpm subcommand (`add` / `remove`), its
/// package spec, and the pnpm log level to forward.
struct PluginCliOp<'a> {
    subcommand: &'a str,
    spec: &'a str,
    loglevel: &'a str,
}

/// Runs one `dsh plugin --profile <name> <pnpm subcommand> <spec/id>` through
/// the instance's own CLI, streaming its output into the task log.
///
/// The launcher still prepares the two things the CLI does not: the
/// build-scripts opt-in (pnpm ≥10 `onlyBuiltDependencies` / pnpm 11
/// `allowBuilds`) and the profile `.npmrc` peer policy. When pnpm 11 blocks
/// build scripts it writes `set this to true or false` placeholders and fails
/// with ERR_PNPM_IGNORED_BUILDS; the placeholders are approved and the
/// invocation is retried once so native deps (node-pty, koffi, esbuild,
/// sharp…) actually build. The CLI prints the same advice for git-hosted
/// plugins, which this automates.
async fn run_dsh_plugin(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    target: &PluginCliTarget<'_>,
    op: &PluginCliOp<'_>,
) -> Result<(), String> {
    let (version_dir, home_path, profile) = (target.version_dir, target.home_path, target.profile);
    let (subcommand, spec, loglevel) = (op.subcommand, op.spec, op.loglevel);
    let dir = profile_dir(home_path, profile);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 profile 目录失败: {e}"))?;
    ensure_build_scripts_allowed(&dir)?;
    // Never let a plugin's peers pull a second copy of a core package in.
    ensure_profile_npmrc(&dir)?;

    let pnpm_prog = ensure_pnpm_for_plugins(app, state, task_id).await?;
    let what = format!("dsh plugin {subcommand}");

    // A node_modules tree linked from a *different* pnpm store (e.g. the
    // profile was first populated by a manual `dsh plugin` run that used the
    // user's global store) makes every pnpm op fail with
    // ERR_PNPM_UNEXPECTED_STORE. Detect the mismatch up front — with
    // `--loglevel=warn` (removals) pnpm prints nothing the log matcher could
    // catch, so the log-based retry below would never fire — and relink.
    let store_dir = state.data_dir.join(".pnpm-store");
    if let Some(linked) = linked_store_dir(&dir) {
        if !store_paths_match(&linked, &store_dir.to_string_lossy()) {
            crate::tasks::push_task_log_pub(
                app,
                state,
                task_id,
                &format!(
                    "node_modules 链接自其他 pnpm store（{linked}），按 pnpm 提示重新链接后重试…"
                ),
            )
            .await;
            relink_profile_store(app, state, task_id, target, &pnpm_prog).await?;
        }
    }

    for attempt in 1..=2 {
        let mut args: Vec<String> = vec![subcommand.to_string(), spec.to_string()];
        args.extend(forwarded_pnpm_flags(state, loglevel, subcommand));
        let cmd = dsh_plugin_command(version_dir, home_path, profile, &args, &pnpm_prog)?;

        match crate::tasks::run_streamed_command(app, state, task_id, cmd, &what).await {
            Ok(()) => return Ok(()),
            Err(_e) if attempt == 1 && task_log_mentions_ignored_builds(state, task_id) => {
                crate::tasks::push_task_log_pub(
                    app,
                    state,
                    task_id,
                    "pnpm 11 拦截了依赖构建脚本，正在批准 allowBuilds 后重试…",
                )
                .await;
                ensure_build_scripts_allowed(&dir)?;
            }
            Err(_e) if attempt == 1 && task_log_mentions_unexpected_store(state, task_id) => {
                crate::tasks::push_task_log_pub(
                    app,
                    state,
                    task_id,
                    "pnpm 报告 store 位置不一致，正在重新链接 node_modules 后重试…",
                )
                .await;
                relink_profile_store(app, state, task_id, target, &pnpm_prog).await?;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("attempt loop covers both attempts")
}

/// Reads the store a profile's `node_modules` is currently linked from, via
/// the `storeDir` line pnpm records in `node_modules/.modules.yaml`.
fn linked_store_dir(profile_dir: &std::path::Path) -> Option<String> {
    let raw =
        std::fs::read_to_string(profile_dir.join("node_modules").join(".modules.yaml")).ok()?;
    for line in raw.lines() {
        if let Some(v) = line.trim().strip_prefix("storeDir:") {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Whether two store paths point at the same store. pnpm records the
/// versioned subdirectory (`<store>/v11`) while the launcher pins the base
/// dir, so a path containing the other as a prefix also counts as a match.
/// Windows filesystems are case-insensitive and pnpm may emit either slash.
fn store_paths_match(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let s = s.replace('/', "\\");
        let s = s.trim_end_matches('\\');
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s.to_string()
        }
    };
    let (a, b) = (norm(a), norm(b));
    a == b || a.starts_with(&format!("{b}\\")) || b.starts_with(&format!("{a}\\"))
}

/// Whether the task's streamed log mentions pnpm's unexpected-store failure
/// (ERR_PNPM_UNEXPECTED_STORE / "Unexpected store location"). Fallback for
/// the proactive `linked_store_dir` check, which only covers stores pnpm
/// recorded in `.modules.yaml`.
fn task_log_mentions_unexpected_store(state: &State<'_, AppState>, task_id: &str) -> bool {
    let tasks = state.tasks.try_lock().map(|t| t.clone()).ok();
    tasks
        .and_then(|t| t.get(task_id).map(|t| t.logs.clone()))
        .map(|logs| {
            logs.iter().any(|l| {
                l.contains("ERR_PNPM_UNEXPECTED_STORE") || l.contains("Unexpected store location")
            })
        })
        .unwrap_or(false)
}

/// Relinks a profile's `node_modules` onto the launcher's pinned store.
/// pnpm's own remedy for ERR_PNPM_UNEXPECTED_STORE is a plain reinstall,
/// which re-imports the lockfile packages from the new store without
/// touching package.json; routing it through `dsh plugin install` keeps the
/// CLI's bundle reconciliation in the loop.
async fn relink_profile_store(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
    target: &PluginCliTarget<'_>,
    pnpm_prog: &std::path::Path,
) -> Result<(), String> {
    let mut args: Vec<String> = vec!["install".to_string()];
    args.extend(forwarded_pnpm_flags(state, "warn", "install"));
    let cmd = dsh_plugin_command(
        target.version_dir,
        target.home_path,
        target.profile,
        &args,
        pnpm_prog,
    )?;
    crate::tasks::run_streamed_command(
        app,
        state,
        task_id,
        cmd,
        "dsh plugin install（重新链接 store）",
    )
    .await
}

/// Whether the task's streamed log mentions pnpm's ignored-build-scripts
/// failure (ERR_PNPM_IGNORED_BUILDS / "Ignored build scripts"). The error
/// text returned by run_streamed_command only carries the last meaningful
/// line, so we inspect the full log instead.
fn task_log_mentions_ignored_builds(state: &State<'_, AppState>, task_id: &str) -> bool {
    let tasks = state.tasks.try_lock().map(|t| t.clone()).ok();
    tasks
        .and_then(|t| t.get(task_id).map(|t| t.logs.clone()))
        .map(|logs| {
            logs.iter().any(|l| {
                l.contains("ERR_PNPM_IGNORED_BUILDS") || l.contains("Ignored build scripts")
            })
        })
        .unwrap_or(false)
}

/// Builds a `dsh plugin --profile <name> <pnpm args…>` invocation for an
/// instance's own CLI version.
///
/// Profile plugin management is a CLI-private flow: `dsh plugin` initializes
/// the profile when needed, forwards the remaining arguments to pnpm with
/// cwd = the profile directory, and then reconciles `dsh.profile.bundles`
/// against the *installed* state (a dependency whose package declares
/// `dsh.bundle.patch` joins the layer stack; one that no longer does leaves
/// it). Driving pnpm ourselves would produce a tree the CLI does not expect
/// and would leave the layer list to be guessed at, so every install and
/// removal goes through the CLI of the version that instance runs.
///
/// The CLI resolves pnpm from PATH, so the launcher's pinned pnpm
/// (`REQUIRED_PNPM_MAJOR`) is prepended to PATH: the pin then also applies
/// inside the CLI's own pnpm invocation.
fn dsh_plugin_command(
    version_dir: &std::path::Path,
    home_path: &std::path::Path,
    profile: &str,
    pnpm_args: &[String],
    pnpm_prog: &std::path::Path,
) -> Result<tokio::process::Command, String> {
    let bin = crate::process::version_bin(version_dir);
    if !crate::process::version_bin_ready(version_dir) {
        return Err(format!(
            "版本安装不完整（缺少 {}），请重新安装该 DSH 版本",
            bin.display()
        ));
    }

    let mut cmd = tokio::process::Command::new(crate::process::node());
    crate::process::hide_console(&mut cmd);
    cmd.arg(&bin)
        .arg("plugin")
        .arg("--profile")
        .arg(profile)
        .args(pnpm_args)
        .env("DSH_HOME", home_path)
        // The launcher can never answer an interactive prompt: pnpm aborts
        // with ERR_PNPM_ABORTED_REMOVE_MODULES_DIR_NO_TTY when it needs to
        // purge a modules dir (store/virtual-store relink) without a TTY.
        // CI=true makes pnpm treat the run as non-interactive instead.
        .env("CI", "true");

    // Prepend the pinned pnpm's directory so the CLI's `spawnSync("pnpm")`
    // picks it up instead of whatever major is on the user's PATH.
    if let Some(pnpm_dir) = pnpm_prog.parent() {
        if !pnpm_dir.as_os_str().is_empty() {
            let existing = std::env::var_os("PATH").unwrap_or_default();
            let mut entries = vec![pnpm_dir.to_path_buf()];
            entries.extend(std::env::split_paths(&existing));
            match std::env::join_paths(entries) {
                Ok(joined) => {
                    cmd.env("PATH", joined);
                }
                Err(e) => {
                    crate::log_warn!("拼接 PATH 失败，沿用系统 PATH: {e}");
                }
            }
        }
    }

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    Ok(cmd)
}

/// Common pnpm flags forwarded through `dsh plugin` (shared store, network
/// robustness, optional registry mirror). `--prefix` is deliberately absent:
/// the CLI already runs pnpm with cwd = the profile directory, and passing a
/// prefix would break that contract.
///
/// The fetch/network flags only exist on download commands (`add` /
/// `install`): `pnpm remove` rejects them outright ("Unknown options:
/// 'fetch-timeout', …") and would fail before touching anything.
fn forwarded_pnpm_flags(
    state: &State<'_, AppState>,
    loglevel: &str,
    subcommand: &str,
) -> Vec<String> {
    let store_dir = state.data_dir.join(".pnpm-store");
    let mut args: Vec<String> = vec![
        "--store-dir".to_string(),
        store_dir.to_string_lossy().to_string(),
        format!("--loglevel={loglevel}"),
    ];
    if subcommand != "remove" {
        args.extend([
            "--fetch-timeout".to_string(),
            "300000".to_string(),
            "--fetch-retries".to_string(),
            "5".to_string(),
            "--fetch-retry-maxtimeout".to_string(),
            "120000".to_string(),
            "--network-concurrency".to_string(),
            "4".to_string(),
        ]);
    }
    if let Ok(registry) = std::env::var("DSH_NPM_REGISTRY") {
        let registry = registry.trim().to_string();
        if !registry.is_empty() {
            args.push("--registry".to_string());
            args.push(registry);
        }
    }
    args
}

/// Pins `auto-install-peers=false` in a profile's `.npmrc`.
///
/// A DSH profile must resolve nothing from the `@deepseek-ai` core scope —
/// core comes from the CLI's own dependency tree. With auto-install-peers on
/// (a common global pnpm setting), installing a plugin whose peers include a
/// core package drops a second copy of that core package into the profile,
/// which is the duplicated-Symbol failure the doctor check reports. Writing
/// the setting per profile makes the install independent of the user's global
/// pnpm configuration.
pub(crate) fn ensure_profile_npmrc(dir: &std::path::Path) -> Result<(), String> {
    const KEY: &str = "auto-install-peers";
    let path = dir.join(".npmrc");
    let raw = if path.exists() {
        std::fs::read_to_string(&path).map_err(|e| format!("读取 .npmrc 失败: {e}"))?
    } else {
        String::new()
    };

    let mut lines: Vec<String> = raw.lines().map(|l| l.to_string()).collect();
    let mut found = false;
    let mut changed = false;
    for line in lines.iter_mut() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        let Some((k, v)) = trimmed.split_once('=') else {
            continue;
        };
        if k.trim() != KEY {
            continue;
        }
        found = true;
        if v.trim() != "false" {
            *line = format!("{KEY}=false");
            changed = true;
        }
    }
    if !found {
        // Keep a short rationale in the file: it is user-visible state.
        if !lines.is_empty() && !lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
            lines.push(String::new());
        }
        lines.push("# DSH: core packages come from the CLI dependency tree;".to_string());
        lines.push("# a profile must never resolve its own copy.".to_string());
        lines.push(format!("{KEY}=false"));
        changed = true;
    }
    if changed {
        let mut out = lines.join("\n");
        out.push('\n');
        std::fs::write(&path, out).map_err(|e| format!("写入 .npmrc 失败: {e}"))?;
    }
    Ok(())
}

/// Make sure a profile's pnpm-workspace.yaml opts into dependency build
/// scripts on both pnpm 10 (onlyBuiltDependencies) and pnpm 11 (allowBuilds).
///
/// pnpm 11 writes `allowBuilds: <name>: set this to true or false` for every
/// dependency whose build script it ignored, then fails the install with
/// ERR_PNPM_IGNORED_BUILDS. This converts those placeholders to `true` and
/// keeps the old field around for pnpm ≤10, so a subsequent install actually
/// runs the native build scripts (node-pty, koffi, esbuild, sharp, …).
pub(crate) fn ensure_build_scripts_allowed(dir: &std::path::Path) -> Result<(), String> {
    let ws_manifest = dir.join("pnpm-workspace.yaml");
    let raw = if ws_manifest.exists() {
        std::fs::read_to_string(&ws_manifest)
            .map_err(|e| format!("读取 pnpm-workspace.yaml 失败: {e}"))?
    } else {
        String::new()
    };

    // Base document: `packages` is required for pnpm to treat the dir as a
    // workspace (needed for allowBuilds to be read from this file).
    let mut lines: Vec<String> = if raw.trim().is_empty() {
        vec!["packages:".to_string(), "  - .".to_string()]
    } else {
        raw.lines().map(|l| l.to_string()).collect()
    };

    // 1. Convert pnpm-11 placeholder values ("set this to true or false") to
    //    real booleans so the next install builds those packages.
    let mut changed = false;
    for line in lines.iter_mut() {
        if line.contains("set this to true or false") {
            *line = line.replace("set this to true or false", "true");
            changed = true;
        }
    }

    // 2. Ensure the legacy `onlyBuiltDependencies: ['*']` block exists
    //    (pnpm ≤10 reads only this field).
    let joined = lines.join("\n");
    if !joined.contains("onlyBuiltDependencies") {
        lines.push(String::new());
        lines.push("onlyBuiltDependencies:".to_string());
        lines.push("  - '*'".to_string());
        changed = true;
    }

    // 3. Ensure an `allowBuilds:` section exists so pnpm 11 has somewhere to
    //    record newly-ignored builds (it auto-appends entries on failure).
    if !lines
        .iter()
        .any(|l| l.trim_start().starts_with("allowBuilds:"))
    {
        lines.push(String::new());
        lines.push("allowBuilds:".to_string());
        changed = true;
    }

    if changed {
        let out = lines.join("\n");
        if !out.ends_with('\n') {
            lines.push(String::new());
        }
        std::fs::write(&ws_manifest, lines.join("\n"))
            .map_err(|e| format!("写入 pnpm-workspace.yaml 失败: {e}"))?;
    }
    Ok(())
}

/// Ensure pnpm is available (delegates to the same logic as version installs).
async fn ensure_pnpm_for_plugins(
    app: &AppHandle,
    state: &State<'_, AppState>,
    task_id: &str,
) -> Result<std::path::PathBuf, String> {
    crate::tasks::ensure_pnpm_pub(app, state, task_id).await
}

/// Ensure cordis.patch.yml has an insert row for the plugin (non-bundle
/// plugins need an explicit mount row).
///
/// The file is a top-level YAML array. A fresh profile ships as comments +
/// a `[]` empty-array placeholder — we must replace that placeholder with a
/// block sequence (`- insert: ...`) instead of appending to it (appending
/// would produce two YAML documents and fail to parse).
fn ensure_cordis_insert(dir: &std::path::Path, plugin_id: &str) -> Result<(), String> {
    let cordis_id = cordis_id_of(plugin_id);
    let path = dir.join("cordis.patch.yml");
    let raw = if path.exists() {
        std::fs::read_to_string(&path).map_err(|e| format!("读取 cordis.patch.yml 失败: {e}"))?
    } else {
        String::new()
    };
    // Already mounted (insert row or a config block for the id)?
    if raw.contains(&format!("id: {cordis_id}")) {
        return Ok(());
    }

    let entry = format!("- insert:\n    - id: {cordis_id}\n      name: '{plugin_id}'\n");

    // Strip comment lines and blank lines to find the actual document body.
    let body: String = raw
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#')
        })
        .collect::<Vec<_>>()
        .join("\n");
    let body_trimmed = body.trim();

    let out = if body_trimmed.is_empty() || body_trimmed == "[]" {
        // Empty document / empty-array placeholder: keep the comment header
        // and replace the `[]` with the new entry.
        let header: String = raw
            .lines()
            .take_while(|l| {
                let t = l.trim();
                t.is_empty() || t.starts_with('#')
            })
            .collect::<Vec<_>>()
            .join("\n");
        if header.trim().is_empty() {
            entry
        } else {
            format!("{}\n{}", header.trim_end(), entry)
        }
    } else {
        // Real entries exist: append a block entry.
        format!("{}\n{}", raw.trim_end(), entry)
    };

    std::fs::write(&path, out).map_err(|e| format!("写入 cordis.patch.yml 失败: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cordis_id_of_strips_scope_and_org() {
        assert_eq!(cordis_id_of("@dsh-plugin/dsh-auxiliary"), "dsh-auxiliary");
        assert_eq!(cordis_id_of("@dsh-external/dsh-sidechain"), "dsh-sidechain");
        assert_eq!(cordis_id_of("dsh-better-sidebar"), "dsh-better-sidebar");
        assert_eq!(cordis_id_of("@canglongcl/dsh-web-review"), "dsh-web-review");
    }

    #[test]
    fn parse_awesome_install_npm_and_github() {
        // npm, bare scoped package.
        assert_eq!(
            parse_awesome_install("dsh plugin --profile web add @furongjun1999/dsh-memory"),
            Some("@furongjun1999/dsh-memory".to_string())
        );
        // npm, unscoped with a version suffix (kept verbatim).
        assert_eq!(
            parse_awesome_install("dsh plugin --profile web add lodash@4.17.21"),
            Some("lodash@4.17.21".to_string())
        );
        // github: spec normalised to owner/repo.
        assert_eq!(
            parse_awesome_install("dsh plugin --profile web add github:0imzero/dsh-workspace-menu"),
            Some("github:0imzero/dsh-workspace-menu".to_string())
        );
        // github: with trailing .git / slash stripped.
        assert_eq!(
            parse_awesome_install("dsh plugin --profile web add github:o/r.git"),
            Some("github:o/r".to_string())
        );
        // A different profile name is fine; only the add target matters.
        assert_eq!(
            parse_awesome_install("dsh plugin --profile tui add @scope/x"),
            Some("@scope/x".to_string())
        );
        // Monorepo subdir: `#path:/…` normalised (leading slash stripped).
        assert_eq!(
            parse_awesome_install(
                "dsh plugin --profile web add github:ayahunter/dsh-trail#path:/packages/bundle"
            ),
            Some("github:ayahunter/dsh-trail#path:packages/bundle".to_string())
        );
        // Subdir without the leading slash is kept as-is.
        assert_eq!(
            parse_awesome_install(
                "dsh plugin --profile web add github:DamonKoy/dsh-web-ui#path:packages/dsh-ssh"
            ),
            Some("github:DamonKoy/dsh-web-ui#path:packages/dsh-ssh".to_string())
        );
    }

    #[test]
    fn parse_github_id_splits_repo_and_subpath() {
        assert_eq!(
            parse_github_id("github:2768651338/dsh-effort-slider"),
            Some(("2768651338/dsh-effort-slider".to_string(), None))
        );
        assert_eq!(
            parse_github_id("github:DamonKoy/dsh-web-ui#path:packages/dsh-task-board"),
            Some((
                "DamonKoy/dsh-web-ui".to_string(),
                Some("packages/dsh-task-board".to_string())
            ))
        );
        // Not github-shaped.
        assert_eq!(parse_github_id("@dsh-plugin/dsh-loader"), None);
        // A committish fragment is not identity — rejected.
        assert_eq!(parse_github_id("github:o/r#main"), None);
        assert_eq!(parse_github_id("github:o/r#path:"), None);
        assert_eq!(parse_github_id("github:onlyowner"), None);
    }

    #[test]
    fn github_install_spec_combines_ref_and_path() {
        assert_eq!(
            github_install_spec("o/r", "b95d997a", None),
            "github:o/r#b95d997a"
        );
        assert_eq!(
            github_install_spec("o/r", "v1.2.3", Some("packages/x")),
            "github:o/r#v1.2.3&path:packages/x"
        );
    }

    #[test]
    fn resolve_installed_name_finds_real_package_name() {
        let dir = std::env::temp_dir().join(format!("dsh-test-resolve-{}", new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{
  "dependencies": {
    "dsh-effort-slider": "github:2768651338/dsh-effort-slider#b95d997a",
    "@dsh-plugin/dsh-loader": "1.3.2",
    "dsh-task-board": "github:DamonKoy/dsh-web-ui#v0.3.0&path:packages/dsh-task-board"
  }
}"#,
        )
        .unwrap();
        // git-hosted: spec recorded verbatim as the value.
        assert_eq!(
            resolve_installed_name(
                &dir,
                "github:2768651338/dsh-effort-slider",
                "github:2768651338/dsh-effort-slider#b95d997a"
            ),
            Some("dsh-effort-slider".to_string())
        );
        // git-hosted monorepo with a normalised (non-verbatim) value: the
        // github:owner/repo prefix still matches.
        assert_eq!(
            resolve_installed_name(
                &dir,
                "github:DamonKoy/dsh-web-ui#path:packages/dsh-task-board",
                "github:DamonKoy/dsh-web-ui#deadbeef&path:packages/dsh-task-board"
            ),
            Some("dsh-task-board".to_string())
        );
        // npm: the id is the key; the spec carries @version.
        assert_eq!(
            resolve_installed_name(
                &dir,
                "@dsh-plugin/dsh-loader",
                "@dsh-plugin/dsh-loader@1.3.2"
            ),
            Some("@dsh-plugin/dsh-loader".to_string())
        );
        // Nothing matches.
        assert_eq!(
            resolve_installed_name(&dir, "lodash", "lodash@4.17.21"),
            None
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn store_paths_match_handles_versioned_subdir_and_slashes() {
        let base = "C:\\Users\\x\\AppData\\Roaming\\in.dsh-plug.dsh-launcher\\.pnpm-store";
        // `.modules.yaml` records the versioned subdir pnpm derived from the
        // pinned base.
        assert!(store_paths_match(&format!("{base}\\v11"), base));
        // Forward slashes and a trailing separator are equivalent.
        assert!(store_paths_match(
            "C:/Users/x/AppData/Roaming/in.dsh-plug.dsh-launcher/.pnpm-store/v11/",
            base
        ));
        // A genuinely different store (the user's global one) mismatches.
        assert!(!store_paths_match(
            "C:\\Users\\x\\AppData\\Local\\pnpm\\store\\v11",
            base
        ));
    }

    #[test]
    fn linked_store_dir_reads_modules_yaml() {
        let dir = std::env::temp_dir().join(format!("dsh-test-modules-{}", new_id("t")));
        let nm = dir.join("node_modules");
        std::fs::create_dir_all(&nm).unwrap();
        std::fs::write(
            nm.join(".modules.yaml"),
            "hoist: true\nstoreDir: C:\\Users\\x\\AppData\\Local\\pnpm\\store\\v11\nvirtualStoreDir: ...\n",
        )
        .unwrap();
        assert_eq!(
            linked_store_dir(&dir).as_deref(),
            Some("C:\\Users\\x\\AppData\\Local\\pnpm\\store\\v11")
        );
        std::fs::remove_dir_all(&dir).ok();
        // Missing file → None (fresh profile, nothing to relink).
        assert_eq!(linked_store_dir(&dir), None);
    }

    #[test]
    fn parse_awesome_install_rejects_undrivable() {
        assert_eq!(parse_awesome_install(""), None);
        assert_eq!(parse_awesome_install("dsh plugin"), None);
        assert_eq!(parse_awesome_install("dsh plugin add"), None);
        // A non-tarball URL is not a registry/github spec we resolve.
        assert_eq!(
            parse_awesome_install("dsh plugin --profile web add https://example.com/x"),
            None
        );
        // github: with a missing repo part.
        assert_eq!(
            parse_awesome_install("dsh plugin add github:onlyowner"),
            None
        );
        // github: with too many path segments.
        assert_eq!(parse_awesome_install("dsh plugin add github:a/b/c"), None);
    }

    #[test]
    fn parse_awesome_install_accepts_tgz_urls() {
        // Quoted tarball URL (GitHub release asset).
        assert_eq!(
            parse_awesome_install(
                "dsh plugin --profile web add \"https://github.com/Crosery/dsh-viewer/releases/latest/download/dsh-viewer.tgz\""
            ),
            Some(
                "tgz:https://github.com/Crosery/dsh-viewer/releases/latest/download/dsh-viewer.tgz"
                    .to_string()
            )
        );
        assert_eq!(
            parse_awesome_install("dsh plugin add https://example.com/pkg.tar.gz"),
            Some("tgz:https://example.com/pkg.tar.gz".to_string())
        );
    }

    #[test]
    fn awesome_to_market_maps_fields() {
        let raw = r#"{
            "name": "dsh-memory",
            "url": "https://github.com/FuRongJun-1999/dsh-memory",
            "category": "agi",
            "description": { "en": "White-box AGI.", "zh": "白箱AGI架构探索。" },
            "npm": "@furongjun1999/dsh-memory",
            "stars": 35,
            "downloads": 1856,
            "install": "dsh plugin --profile web add @furongjun1999/dsh-memory",
            "added": "2026-08-14"
        }"#;
        let aw: AwesomePlugin = serde_json::from_str(raw).unwrap();
        let mp = awesome_to_market(&aw).expect("install resolves");
        assert_eq!(mp.id, "@furongjun1999/dsh-memory");
        assert_eq!(mp.name, "dsh-memory");
        assert_eq!(mp.source, PluginSource::AwesomeDshPlugin);
        assert_eq!(mp.category.as_deref(), Some("agi"));
        assert_eq!(mp.stars, Some(35));
        assert_eq!(mp.downloads, Some(1856));
        assert_eq!(
            mp.urls.as_ref().unwrap().repository.as_deref(),
            Some("https://github.com/FuRongJun-1999/dsh-memory")
        );
        match mp.description {
            Some(MarketDescription::Localized(list)) => {
                assert_eq!(list.len(), 2);
                assert!(list.iter().any(|d| d.language == "zh"));
            }
            other => panic!("expected localized description, got {other:?}"),
        }
    }

    #[test]
    fn awesome_to_market_github_entry() {
        let raw = r#"{
            "name": "dsh-workspace-menu",
            "url": "https://github.com/0imzero/dsh-workspace-menu",
            "npm": null,
            "stars": null,
            "downloads": null,
            "install": "dsh plugin --profile web add github:0imzero/dsh-workspace-menu"
        }"#;
        let aw: AwesomePlugin = serde_json::from_str(raw).unwrap();
        let mp = awesome_to_market(&aw).expect("github install resolves");
        assert_eq!(mp.id, "github:0imzero/dsh-workspace-menu");
        assert!(mp.description.is_none(), "no description block");
        assert_eq!(mp.stars, None);
    }

    #[test]
    fn set_disabled_row_adds_and_removes() {
        let raw = "# comment\n- id: other-plugin\n  config:\n    a: 1\n";
        // Add a disable row for dsh-auxiliary.
        let out = set_disabled_row(raw, "dsh-auxiliary", false);
        assert!(out.contains("- id: dsh-auxiliary"), "out: {out}");
        assert!(out.contains("  disabled: true"), "out: {out}");
        // The unrelated block must be preserved.
        assert!(out.contains("other-plugin"), "out: {out}");
        assert!(out.contains("config"), "out: {out}");
        assert!(out.contains("a: 1"), "out: {out}");

        // Remove it again -> back to the original content.
        let back = set_disabled_row(&out, "dsh-auxiliary", true);
        assert!(!back.contains("dsh-auxiliary"), "back: {back}");
        assert!(back.contains("other-plugin"), "back: {back}");
        assert!(back.contains("config"), "back: {back}");
    }

    #[test]
    fn set_disabled_row_replaces_existing() {
        let raw = "- id: dsh-auxiliary\n  disabled: true\n";
        let out = set_disabled_row(raw, "dsh-auxiliary", true);
        assert!(!out.contains("dsh-auxiliary"), "out: {out}");
        // Re-disable after removal.
        let out2 = set_disabled_row(&out, "dsh-auxiliary", false);
        assert!(out2.contains("- id: dsh-auxiliary"), "out2: {out2}");
        assert!(out2.contains("  disabled: true"), "out2: {out2}");
    }

    #[test]
    fn read_disabled_ids_parses_blocks() {
        let dir = std::env::temp_dir().join(format!("dsh-plugins-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("cordis.patch.yml"),
            "# header\n- id: ui-dsh-aionui-panel\n  disabled: true\n\n- id: live-stats\n  disabled: true\n\n- id: keep\n  config:\n    x: 1\n",
        )
        .unwrap();
        let set = read_disabled_ids(&dir);
        assert!(set.contains("ui-dsh-aionui-panel"));
        assert!(set.contains("live-stats"));
        assert!(!set.contains("keep"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn profile_lock_key_matches_paths_that_denote_one_profile() {
        // Two instances sharing a DSH_HOME must contend for the same lock, so
        // the key normalizes case on Windows (case-insensitive filesystem).
        let a = std::path::Path::new("C:\\homes\\lab\\profiles\\web");
        let b = std::path::Path::new("C:\\Homes\\Lab\\Profiles\\Web");
        if cfg!(windows) {
            assert_eq!(profile_lock_key(a), profile_lock_key(b));
        } else {
            assert_ne!(profile_lock_key(a), profile_lock_key(b));
        }
        // Different profiles under one HOME never share a lock.
        let other = std::path::Path::new("C:\\homes\\lab\\profiles\\tui");
        assert_ne!(profile_lock_key(a), profile_lock_key(other));
    }

    #[test]
    fn ensure_cordis_insert_only_once() {
        let dir = std::env::temp_dir().join(format!("dsh-plugins-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        ensure_cordis_insert(&dir, "@dsh-plugin/dsh-auxiliary").unwrap();
        ensure_cordis_insert(&dir, "@dsh-plugin/dsh-auxiliary").unwrap();
        let raw = std::fs::read_to_string(dir.join("cordis.patch.yml")).unwrap();
        assert_eq!(raw.matches("- insert:").count(), 1, "raw: {raw}");
        assert!(raw.contains("id: dsh-auxiliary"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn manifest_lists_bundle_reads_the_cli_reconciled_layer_list() {
        // Bundle registration is the CLI's verdict: after `dsh plugin add`
        // reconciles dsh.profile.bundles, the launcher only reads it back to
        // decide whether an extra cordis insert row is needed.
        let dir = std::env::temp_dir().join(format!("dsh-plugins-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // No manifest yet: the default has an empty bundle list.
        assert!(!manifest_lists_bundle(&dir, "@dsh-plugin/dsh-auxiliary").unwrap());

        // Reconciled into the layer stack -> listed.
        std::fs::write(
            dir.join("package.json"),
            r#"{"private":true,"dependencies":{"@dsh-plugin/dsh-auxiliary":"^0.5.1","@dsh-plugin/plain":"^1.0.0"},"dsh":{"profile":{"bundles":["@deepseek-ai/dsh-base","@dsh-plugin/dsh-auxiliary"]}}}"#,
        )
        .unwrap();
        assert!(manifest_lists_bundle(&dir, "@dsh-plugin/dsh-auxiliary").unwrap());
        // A dependency the CLI did not reconcile (no dsh.bundle.patch) needs
        // the explicit insert row instead.
        assert!(!manifest_lists_bundle(&dir, "@dsh-plugin/plain").unwrap());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ensure_cordis_insert_replaces_empty_array_placeholder() {
        // The real-world bug: a fresh profile ships comments + `[]` and the
        // first insert row must REPLACE `[]`, not append after it (two YAML
        // documents would otherwise fail to parse).
        let dir = std::env::temp_dir().join(format!("dsh-plugins-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("cordis.patch.yml"),
            "# cordis.patch.yml\n# a top-level YAML array of load-order\n# overrides\n[]\n",
        )
        .unwrap();
        ensure_cordis_insert(&dir, "@dsh-plugin/dsh-auxiliary").unwrap();
        let raw = std::fs::read_to_string(dir.join("cordis.patch.yml")).unwrap();
        assert!(raw.contains("# cordis.patch.yml"), "header kept: {raw}");
        assert!(!raw.contains("[]"), "placeholder replaced: {raw}");
        assert!(raw.contains("- insert:"), "raw: {raw}");
        assert!(raw.contains("id: dsh-auxiliary"), "raw: {raw}");
        // The body must be a single valid block sequence.
        let body: String = raw
            .lines()
            .filter(|l| !l.trim().starts_with('#') && !l.trim().is_empty())
            .collect();
        assert!(body.starts_with("- insert:"), "single sequence: {body}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn set_disabled_row_replaces_empty_array_placeholder() {
        let raw = "# header\n# comment\n[]\n";
        let out = set_disabled_row(raw, "dsh-auxiliary", false);
        assert!(out.contains("# header"), "header kept: {out}");
        assert!(!out.contains("[]"), "placeholder dropped: {out}");
        assert!(out.contains("- id: dsh-auxiliary"), "out: {out}");
        assert!(out.contains("  disabled: true"), "out: {out}");
    }

    #[test]
    fn set_disabled_row_empty_again_restores_placeholder() {
        // Disabling then re-enabling the only entry should leave a valid
        // document again (comment header + `[]`), not an empty file.
        let raw = "# header\n[]\n";
        let off = set_disabled_row(raw, "dsh-auxiliary", false);
        assert!(!off.contains("[]"));
        let on = set_disabled_row(&off, "dsh-auxiliary", true);
        assert!(on.contains("[]"), "placeholder restored: {on}");
        assert!(!on.contains("dsh-auxiliary"), "entry removed: {on}");
    }

    #[test]
    fn strip_cordis_rows_removes_insert_and_disabled_blocks() {
        // A plugin mounted via an insert row plus a disabled row for another
        // plugin must leave the other plugin intact.
        let raw = "# header\n- insert:\n    - id: dsh-auxiliary\n      name: '@dsh-plugin/dsh-auxiliary'\n\n- id: dsh-thought-buddy\n  disabled: true\n\n- id: keep\n  config:\n    x: 1\n";
        let out = strip_cordis_rows(raw, "dsh-auxiliary", "@dsh-plugin/dsh-auxiliary");
        assert!(!out.contains("dsh-auxiliary"), "insert row removed: {out}");
        assert!(out.contains("dsh-thought-buddy"), "other block kept: {out}");
        assert!(out.contains("keep"), "config block kept: {out}");
        assert!(out.contains("x: 1"), "config content kept: {out}");
    }

    #[test]
    fn strip_cordis_rows_restores_placeholder_when_empty() {
        let raw = "# header\n- id: dsh-auxiliary\n  disabled: true\n";
        let out = strip_cordis_rows(raw, "dsh-auxiliary", "@dsh-plugin/dsh-auxiliary");
        assert!(out.contains("[]"), "placeholder restored: {out}");
        assert!(!out.contains("dsh-auxiliary"), "entry removed: {out}");
    }

    #[test]
    fn relationship_type_alias_roundtrip() {
        // The market JSON uses `type`, the frontend expects `kind`.
        let raw = r#"{"type":"dependency","id":"@dsh-plugin/dsh-loader","versions":">=1.3.0"}"#;
        let rel: MarketPluginRelationship = serde_json::from_str(raw).unwrap();
        assert_eq!(rel.kind, "dependency");
        assert_eq!(rel.id, "@dsh-plugin/dsh-loader");
        // Serialized back out it must be `kind` (frontend contract).
        let out = serde_json::to_string(&rel).unwrap();
        assert!(out.contains("\"kind\":\"dependency\""), "out: {out}");
        assert!(!out.contains("\"type\":"), "out: {out}");
    }

    // Live network smoke tests (skipped by default; run with
    // `cargo test plugins::tests::live_ -- --ignored`).
    #[tokio::test]
    #[ignore]
    async fn live_fetch_market_and_versions() {
        let plugins = fetch_plugin_market(None).await.unwrap();
        assert!(!plugins.is_empty(), "market must return plugins");
        // The catalog must contain the loader plugin.
        assert!(
            plugins.iter().any(|p| p.id == "@dsh-plugin/dsh-loader"),
            "loader missing from market"
        );
        // Every relationship must round-trip to `kind` for the frontend.
        for p in &plugins {
            if let Some(rels) = &p.relationship {
                for r in rels {
                    let out = serde_json::to_string(r).unwrap();
                    assert!(
                        out.contains("\"kind\":"),
                        "relationship of {} must serialize kind: {out}",
                        p.id
                    );
                    assert!(
                        !out.contains("\"type\":"),
                        "relationship of {} must not leak `type`: {out}",
                        p.id
                    );
                }
            }
        }
        // npm-based stable versions for a known plugin.
        let stable = npm_versions("@dsh-plugin/dsh-auxiliary", &PluginChannel::Stable)
            .await
            .unwrap();
        assert!(!stable.is_empty());
        assert!(stable.iter().any(|v| v.is_default));
        let beta = npm_versions("@dsh-plugin/dsh-auxiliary", &PluginChannel::Beta)
            .await
            .unwrap();
        assert!(!beta.is_empty());
        // alpha: GitHub commit channel (client_id boosts the rate limit).
        let page1 = fetch_plugin_versions(
            "@dsh-plugin/dsh-auxiliary".to_string(),
            PluginChannel::Alpha,
            Some(1),
        )
        .await
        .unwrap();
        assert!(
            !page1.versions.is_empty(),
            "alpha commits must be fetchable"
        );
        assert!(page1.versions[0].is_default, "first commit is the default");
        // Pagination: page 2 must return a disjoint set when has_more.
        if page1.has_more {
            let page2 = fetch_plugin_versions(
                "@dsh-plugin/dsh-auxiliary".to_string(),
                PluginChannel::Alpha,
                Some(2),
            )
            .await
            .unwrap();
            assert!(!page2.versions.is_empty());
            assert!(
                page2
                    .versions
                    .iter()
                    .all(|v| !page1.versions.iter().any(|a| a.version == v.version)),
                "page 2 must not repeat page 1 commits"
            );
            assert!(!page2.versions[0].is_default, "only page 1 has the default");
        }
    }

    #[test]
    fn ensure_build_scripts_allowed_converts_placeholders_and_adds_sections() {
        let dir = std::env::temp_dir().join(format!("dsh-plugins-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // Fresh profile: no workspace file yet -> packages + both sections.
        ensure_build_scripts_allowed(&dir).unwrap();
        let fresh = std::fs::read_to_string(dir.join("pnpm-workspace.yaml")).unwrap();
        assert!(fresh.contains("packages:"), "fresh: {fresh}");
        assert!(fresh.contains("onlyBuiltDependencies"), "fresh: {fresh}");
        assert!(fresh.contains("allowBuilds:"), "fresh: {fresh}");

        // pnpm 11 left a placeholder behind after ERR_PNPM_IGNORED_BUILDS.
        std::fs::write(
            dir.join("pnpm-workspace.yaml"),
            "packages:\n  - .\nallowBuilds:\n  node-pty: set this to true or false\n",
        )
        .unwrap();
        ensure_build_scripts_allowed(&dir).unwrap();
        let fixed = std::fs::read_to_string(dir.join("pnpm-workspace.yaml")).unwrap();
        assert!(fixed.contains("node-pty: true"), "fixed: {fixed}");
        assert!(
            !fixed.contains("set this to true or false"),
            "fixed: {fixed}"
        );
        // Legacy section added without clobbering existing content.
        assert!(fixed.contains("onlyBuiltDependencies"), "fixed: {fixed}");

        // Idempotent: second run leaves the file unchanged.
        let before = std::fs::read_to_string(dir.join("pnpm-workspace.yaml")).unwrap();
        ensure_build_scripts_allowed(&dir).unwrap();
        let after = std::fs::read_to_string(dir.join("pnpm-workspace.yaml")).unwrap();
        assert_eq!(before, after, "must be idempotent");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ensure_profile_npmrc_pins_auto_install_peers_false() {
        let dir = std::env::temp_dir().join(format!("dsh-npmrc-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let npmrc = dir.join(".npmrc");

        // Fresh profile: the key is written.
        ensure_profile_npmrc(&dir).unwrap();
        let fresh = std::fs::read_to_string(&npmrc).unwrap();
        assert!(fresh.contains("auto-install-peers=false"), "fresh: {fresh}");

        // Idempotent.
        ensure_profile_npmrc(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(&npmrc).unwrap(), fresh);

        // An opposite existing value is normalized, other keys are preserved.
        std::fs::write(
            &npmrc,
            "registry=https://example.com/\nauto-install-peers=true\n",
        )
        .unwrap();
        ensure_profile_npmrc(&dir).unwrap();
        let fixed = std::fs::read_to_string(&npmrc).unwrap();
        assert!(fixed.contains("auto-install-peers=false"), "fixed: {fixed}");
        assert!(!fixed.contains("auto-install-peers=true"), "fixed: {fixed}");
        assert!(
            fixed.contains("registry=https://example.com/"),
            "other keys must survive: {fixed}"
        );

        // A commented-out key is not treated as set.
        std::fs::write(&npmrc, "# auto-install-peers=true\n").unwrap();
        ensure_profile_npmrc(&dir).unwrap();
        let commented = std::fs::read_to_string(&npmrc).unwrap();
        assert!(
            commented.contains("\nauto-install-peers=false"),
            "commented: {commented}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
