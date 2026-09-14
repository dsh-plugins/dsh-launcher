// Plugin marketplace: fetches the plugin catalog from the market API and
// exposes per-channel versions (stable = releases/latest, beta =
// pre-releases/next, alpha = latest commit) plus install/enable plumbing.

use crate::config::{
    default_plugin_sources, new_id, sanitize_name, Confidence, DshInstance, PluginSourceConfig,
    SourceKind,
};
use crate::AppState;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

const NPM_REGISTRY: &str = "https://registry.npmjs.org";

/// Environment override for the enabled plugin sources (issue #46). Comma-
/// separated entries; each is either `url` or `id|kind|url`. Replaces the
/// persisted list for this process only (never written back to config.json).
const PLUGIN_SOURCES_ENV: &str = "DSHLAUNCHER_PLUGIN_SOURCES";

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
    /// Id of the source this entry came from (issue #46): a built-in id like
    /// "dsh-plugins" / "awesome-dsh-plugin" / "dshget" / "github-topic", or a
    /// user-defined id. Defaults to the primary catalog so old cached/frontend
    /// payloads without the field stay valid.
    #[serde(default = "default_source_id")]
    pub source: String,
    /// Credibility tier of the winning source; see `Confidence`.
    #[serde(default)]
    pub confidence: Confidence,
    /// Every source id that lists this plugin (attribution union after dedup).
    #[serde(default)]
    pub sources: Vec<String>,
    /// `owner/repo` hint so version resolution (alpha channel) and alpha
    /// installs work for entries that are not in any static catalog.
    #[serde(default)]
    pub repo: Option<String>,
    /// Upstream verification status (dshget), passed through verbatim.
    #[serde(default)]
    pub verification: Option<String>,
    /// Community-catalog extras (absent for the primary catalog).
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub stars: Option<u64>,
    #[serde(default)]
    pub downloads: Option<u64>,
}

fn default_source_id() -> String {
    "dsh-plugins".to_string()
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

/// Builds a localized bilingual description out of an `{ en, zh }` pair.
fn description_from(en: Option<&str>, zh: Option<&str>) -> Option<MarketDescription> {
    let mut list = Vec::new();
    if let Some(en) = en {
        list.push(MarketPluginDescription {
            language: "en".to_string(),
            content: en.to_string(),
        });
    }
    if let Some(zh) = zh {
        list.push(MarketPluginDescription {
            language: "zh".to_string(),
            content: zh.to_string(),
        });
    }
    if list.is_empty() {
        None
    } else {
        Some(MarketDescription::Localized(list))
    }
}

/// Best-effort `owner/repo` hint from a catalog entry: the install target id
/// (for `github:` ids) or the entry's repository/page URL.
fn repo_hint_of(id: &str, url: Option<&str>) -> Option<String> {
    if let Some((repo, _)) = parse_github_id(id) {
        return Some(repo);
    }
    let url = url?;
    if let Some(pos) = url.find("github.com/") {
        let tail = &url[pos + "github.com/".len()..];
        let tail = tail.trim_end_matches(".git").trim_end_matches('/');
        let mut parts = tail.split('/');
        if let (Some(owner), Some(name)) = (parts.next(), parts.next()) {
            if !owner.is_empty() && !name.is_empty() {
                return Some(format!("{owner}/{name}"));
            }
        }
    }
    None
}

/// Stamps an entry with the source it came from and that source's credibility.
fn tag_entry(mp: &mut MarketPlugin, src: &PluginSourceConfig) {
    mp.source = src.id.clone();
    mp.confidence = src.confidence;
    if !mp.sources.contains(&src.id) {
        mp.sources.insert(0, src.id.clone());
    }
}

/// Converts one awesome-dsh-plugin entry into a `MarketPlugin`, or None when
/// its `install` line cannot be resolved to a drivable plugin id.
fn awesome_to_market(p: &AwesomePlugin) -> Option<MarketPlugin> {
    let id = parse_awesome_install(&p.install)?;
    let description = p
        .description
        .as_ref()
        .and_then(|d| description_from(d.en.as_deref(), d.zh.as_deref()));
    let urls = p.url.as_ref().map(|u| MarketPluginUrls {
        homepage: None,
        repository: Some(u.clone()),
        issues: None,
    });
    let repo = repo_hint_of(&id, p.url.as_deref());
    Some(MarketPlugin {
        id,
        name: p.name.clone(),
        description,
        support_versions: None,
        urls,
        relationship: None,
        source: default_source_id(),
        confidence: Confidence::default(),
        sources: Vec::new(),
        repo,
        verification: None,
        category: p.category.clone(),
        stars: p.stars,
        downloads: p.downloads,
    })
}

// ---------------------------------------------------------------------------
// DSH Get catalog (dshget-data/catalog.json)
// ---------------------------------------------------------------------------

/// One entry in the DSH Get aggregated catalog. Only the fields the launcher
/// consumes are modelled; `install` uses the same shape as awesome's, so
/// `parse_awesome_install` handles it directly.
#[derive(Clone, Debug, Deserialize)]
struct DshGetPlugin {
    name: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    description: Option<AwesomeDescription>,
    #[serde(default)]
    stars: Option<u64>,
    /// e.g. `dsh plugin --profile web add github:owner/repo`.
    install: String,
    /// Upstream catalogs this entry was aggregated from (attribution).
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    verification: Option<String>,
    #[serde(default = "default_true")]
    installable: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct DshGetCatalog {
    #[serde(default)]
    plugins: Vec<DshGetPlugin>,
}

fn default_true() -> bool {
    true
}

/// Converts one DSH Get entry into a `MarketPlugin`. Entries the catalog marks
/// as not installable are dropped.
fn dshget_to_market(p: &DshGetPlugin) -> Option<MarketPlugin> {
    if !p.installable {
        return None;
    }
    let id = parse_awesome_install(&p.install)?;
    let description = p
        .description
        .as_ref()
        .and_then(|d| description_from(d.en.as_deref(), d.zh.as_deref()));
    let urls = p.url.as_ref().map(|u| MarketPluginUrls {
        homepage: None,
        repository: Some(u.clone()),
        issues: None,
    });
    let repo = repo_hint_of(&id, p.url.as_deref());
    Some(MarketPlugin {
        id,
        name: p.name.clone(),
        description,
        support_versions: None,
        urls,
        relationship: None,
        source: default_source_id(),
        confidence: Confidence::default(),
        sources: p.sources.clone(),
        repo,
        verification: p.verification.clone(),
        category: p.category.clone(),
        stars: p.stars,
        downloads: None,
    })
}

// ---------------------------------------------------------------------------
// GitHub topic live discovery (issue #46, channel C)
// ---------------------------------------------------------------------------

const TOPIC_QUERY: &str = "topic:dsh-plugin";
const TOPIC_PER_PAGE: u32 = 100;
/// Page budget: the search API is heavily rate-limited (~30 req/min), so we
/// never page deeper than this per refresh.
const TOPIC_MAX_PAGES: u32 = 2;
/// Probe budget: repos whose name is not self-identifying are checked against
/// their raw manifest, but only this many per refresh.
const TOPIC_MAX_PROBES: usize = 30;

/// Repos/orgs that carry the topic but are not installable plugins: DeepSeek's
/// own core repos, catalogs/aggregators, and the launcher itself. Matched
/// case-insensitively against `owner/repo` (exact, or as an owner prefix).
const TOPIC_DENYLIST: &[&str] = &[
    "deepseek-ai",
    "dsh-plugins/dsh-launcher",
    "bobby-sheng/dshget-data",
    "bobby-sheng/dshget-plugin",
    "omdsh-dev/dsh-hub-workshop",
    "hrhgit/deepseek-harness-plugin-manager",
];

fn topic_denied(full_name: &str) -> bool {
    let lower = full_name.to_lowercase();
    let owner = lower.split('/').next().unwrap_or("");
    TOPIC_DENYLIST.iter().any(|d| {
        let d = d.to_lowercase();
        lower == d || owner == d || lower.starts_with(&format!("{d}/"))
    })
}

/// Strong heuristic: the repo name announces itself as a DSH plugin.
fn name_looks_like_plugin(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with("dsh-") || n.starts_with("dsh_") || n.contains("dsh-plugin")
}

/// Pure denoise decision for one topic candidate: it must not be denylisted and
/// must either self-identify by name or carry a DSH plugin manifest (checked
/// separately over the network).
fn topic_entry_accepted(name: &str, owner: &str, has_manifest: bool) -> bool {
    let full = format!("{owner}/{name}");
    if topic_denied(&full) || is_core_package(&format!("github:{full}")) {
        return false;
    }
    name_looks_like_plugin(name) || has_manifest
}

/// Fetches one search page with bounded exponential backoff.
async fn topic_search_page(page: u32) -> Result<Vec<serde_json::Value>, String> {
    let url = github_api_url(&format!(
        "/search/repositories?q={TOPIC_QUERY}&sort=updated&order=desc&per_page={TOPIC_PER_PAGE}&page={page}"
    ));
    let mut delay_ms = 1000u64;
    let mut last_err = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            delay_ms *= 2;
        }
        match fetch_json(&url, 8 * 1024 * 1024).await {
            Ok(v) => {
                return Ok(v
                    .get("items")
                    .and_then(|i| i.as_array())
                    .cloned()
                    .unwrap_or_default())
            }
            Err(e) => {
                crate::log_warn!(
                    "GitHub topic 搜索第 {page} 页失败(第 {} 次): {e}",
                    attempt + 1
                );
                last_err = e;
            }
        }
    }
    Err(last_err)
}

/// Probes a repo's default branch (`HEAD` on raw.githubusercontent, so no extra
/// API call) for a DSH plugin manifest: `package.json` declaring a `dsh.bundle`,
/// or a `cordis.patch.yml`.
async fn repo_has_dsh_manifest(full_name: &str) -> bool {
    // Probes are best-effort and numerous, so use a much shorter timeout than
    // the catalog fetches: a stalled CDN must not stall the whole refresh.
    let Ok(client) = crate::proxy::apply(reqwest::Client::builder())
        .timeout(std::time::Duration::from_secs(8))
        .user_agent("dsh-launcher")
        .build()
    else {
        return false;
    };
    let pkg_url = format!("https://cdn.jsdelivr.net/gh/{full_name}@HEAD/package.json");
    if let Ok(resp) = client.get(&pkg_url).send().await {
        if resp.status().is_success() {
            if let Ok(bytes) = resp.bytes().await {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    let has_bundle = v.get("dsh.bundle").is_some()
                        || v.get("dsh")
                            .and_then(|d| d.get("bundle"))
                            .is_some_and(|b| !b.is_null());
                    if has_bundle {
                        return true;
                    }
                }
            }
        }
    }
    let patch_url = format!("https://cdn.jsdelivr.net/gh/{full_name}@HEAD/cordis.patch.yml");
    matches!(client.get(&patch_url).send().await, Ok(r) if r.status().is_success())
}

/// Live discovery over `topic:dsh-plugin`. Pages are budgeted with backoff; a
/// later page failing keeps the pages already collected (resume semantics)
/// instead of discarding the refresh, and the caller's cache wrapper handles
/// TTL + last-good.
async fn fetch_github_topic() -> Result<Vec<MarketPlugin>, String> {
    let mut repos: Vec<serde_json::Value> = Vec::new();
    for page in 1..=TOPIC_MAX_PAGES {
        match topic_search_page(page).await {
            Ok(items) => {
                let full_page = items.len() == TOPIC_PER_PAGE as usize;
                repos.extend(items);
                if !full_page {
                    break;
                }
            }
            Err(e) => {
                if repos.is_empty() {
                    return Err(e);
                }
                crate::log_warn!(
                    "topic 第 {page} 页失败，保留已获取的 {} 条候选: {e}",
                    repos.len()
                );
                break;
            }
        }
    }
    if repos.is_empty() {
        return Err("GitHub topic 搜索未返回结果".to_string());
    }

    let mut out = Vec::new();
    let mut probes = 0usize;
    let mut probed_out = 0usize;
    for r in &repos {
        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let full_name = r.get("full_name").and_then(|v| v.as_str()).unwrap_or("");
        if name.is_empty() || full_name.is_empty() {
            continue;
        }
        let mut accepted = name_looks_like_plugin(name);
        if !accepted && !topic_denied(full_name) {
            if probes < TOPIC_MAX_PROBES {
                probes += 1;
                accepted = repo_has_dsh_manifest(full_name).await;
            } else {
                probed_out += 1;
                continue;
            }
        }
        let owner = full_name.split('/').next().unwrap_or("");
        if !topic_entry_accepted(name, owner, accepted) {
            continue;
        }
        let description = r
            .get("description")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let html = r.get("html_url").and_then(|v| v.as_str());
        out.push(MarketPlugin {
            id: format!("github:{full_name}"),
            name: name.to_string(),
            description: description.map(|d| MarketDescription::Plain(d.to_string())),
            support_versions: None,
            urls: html.map(|u| MarketPluginUrls {
                homepage: None,
                repository: Some(u.to_string()),
                issues: None,
            }),
            relationship: None,
            source: default_source_id(),
            confidence: Confidence::default(),
            sources: Vec::new(),
            repo: Some(full_name.to_string()),
            verification: None,
            category: None,
            stars: r.get("stargazers_count").and_then(|v| v.as_u64()),
            downloads: None,
        });
    }
    if probed_out > 0 {
        crate::log_warn!("topic 通道探测预算用尽，{probed_out} 个候选未验证被跳过");
    }
    Ok(out)
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
    /// `owner/repo` (or a GitHub URL) hint for alpha installs of npm-id plugins
    /// that live in a repo; required for entries found via a live source.
    #[serde(default)]
    pub repo: Option<String>,
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

/// Stamps a whole listing with its source. Every adapter funnels through this,
/// so a new adapter cannot forget to attribute its entries (which would make
/// them masquerade as the official catalog).
fn tag_all(src: &PluginSourceConfig, mut list: Vec<MarketPlugin>) -> Vec<MarketPlugin> {
    for p in &mut list {
        tag_entry(p, src);
    }
    list
}

/// Adapter dispatch: fetches and parses one catalog over the network, tagging
/// every entry with the source id + credibility. No caching here (see
/// `fetch_source`).
async fn fetch_catalog(src: &PluginSourceConfig) -> Result<Vec<MarketPlugin>, String> {
    match src.kind {
        SourceKind::Primary => {
            let v = fetch_json(&src.url, 8 * 1024 * 1024).await?;
            let list: Vec<MarketPlugin> =
                serde_json::from_value(v).map_err(|e| format!("解析主源数据失败: {e}"))?;
            Ok(tag_all(src, list))
        }
        SourceKind::Awesome => {
            let v = fetch_json(&src.url, 8 * 1024 * 1024).await?;
            let cat: AwesomeCatalog =
                serde_json::from_value(v).map_err(|e| format!("解析 awesome 数据失败: {e}"))?;
            let mut out = Vec::new();
            for aw in &cat.plugins {
                match awesome_to_market(aw) {
                    Some(mp) => out.push(mp),
                    None => crate::log_warn!(
                        "awesome 条目「{}」install 行无法解析，跳过: {}",
                        aw.name,
                        aw.install
                    ),
                }
            }
            Ok(tag_all(src, out))
        }
        SourceKind::DshGet => {
            let v = fetch_json(&src.url, 8 * 1024 * 1024).await?;
            let cat: DshGetCatalog =
                serde_json::from_value(v).map_err(|e| format!("解析 dshget 数据失败: {e}"))?;
            let mut out = Vec::new();
            for p in &cat.plugins {
                if let Some(mp) = dshget_to_market(p) {
                    out.push(mp);
                }
            }
            Ok(tag_all(src, out))
        }
        SourceKind::GithubTopic => Ok(tag_all(src, fetch_github_topic().await?)),
    }
}

// Per-source last-good disk cache, so an unreachable source degrades instead of
// disappearing from the market. Live sources reuse a fresh cache to respect the
// GitHub search rate limit.
const TOPIC_CACHE_TTL_SECS: i64 = 24 * 60 * 60;

#[derive(Serialize, Deserialize)]
struct SourceCache {
    saved_at: i64,
    plugins: Vec<MarketPlugin>,
}

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Cache file for one source. The id is sanitised for filesystem safety AND
/// hashed: ids are user-supplied and distinct ids can sanitise to the same name
/// (e.g. `a/b` and `a_b`), which would let sources serve each other's cache.
fn cache_file(cache_dir: &std::path::Path, id: &str) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(id.as_bytes());
    let hash = digest
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    cache_dir.join(format!("{}-{hash}.json", sanitize_name(id)))
}

fn read_source_cache(cache_dir: &std::path::Path, id: &str) -> Option<SourceCache> {
    let raw = std::fs::read_to_string(cache_file(cache_dir, id)).ok()?;
    serde_json::from_str::<SourceCache>(&raw).ok()
}

fn write_source_cache(cache_dir: &std::path::Path, id: &str, plugins: &[MarketPlugin]) {
    if std::fs::create_dir_all(cache_dir).is_err() {
        return;
    }
    let payload = SourceCache {
        saved_at: now_ts(),
        plugins: plugins.to_vec(),
    };
    if let Ok(raw) = serde_json::to_string(&payload) {
        let _ = std::fs::write(cache_file(cache_dir, id), raw);
    }
}

/// Fetches one source, degrading to its last-good cache when the network fails.
async fn fetch_source(
    src: &PluginSourceConfig,
    cache_dir: Option<&std::path::Path>,
) -> Result<Vec<MarketPlugin>, String> {
    if src.kind == SourceKind::GithubTopic {
        if let Some(dir) = cache_dir {
            if let Some(c) = read_source_cache(dir, &src.id) {
                if now_ts() - c.saved_at < TOPIC_CACHE_TTL_SECS {
                    return Ok(c.plugins);
                }
            }
        }
    }
    match fetch_catalog(src).await {
        Ok(list) => {
            if let Some(dir) = cache_dir {
                write_source_cache(dir, &src.id, &list);
            }
            Ok(list)
        }
        Err(e) => {
            if let Some(dir) = cache_dir {
                if let Some(c) = read_source_cache(dir, &src.id) {
                    crate::log_warn!(
                        "插件源「{}」不可达，改用 last-good 缓存({} 条): {e}",
                        src.id,
                        c.plugins.len()
                    );
                    return Ok(c.plugins);
                }
            }
            Err(e)
        }
    }
}

/// Whether `owner/repo` belongs to DeepSeek's own org.
fn is_core_repo(repo: &str) -> bool {
    repo.split('/')
        .next()
        .is_some_and(|owner| owner.eq_ignore_ascii_case("deepseek-ai"))
}

/// The grain red line (issue #46): never surface DeepSeek's own core packages
/// as installable marketplace plugins, whatever source produced them. Matching
/// is normalised (trimmed, `npm:` prefix stripped, case-insensitive) so a
/// source cannot slip one past with cosmetic differences.
fn is_core_package(id: &str) -> bool {
    let id = id.trim();
    let id = id.strip_prefix("npm:").unwrap_or(id);
    if id.to_lowercase().starts_with("@deepseek-ai/") {
        return true;
    }
    parse_github_id(id).is_some_and(|(repo, _)| is_core_repo(&repo))
}

/// Drops an entry if either its install id or its repo hint points at a core
/// package: the hint is what alpha resolution/installs follow, so a benign id
/// paired with a core repo must not get through.
fn entry_is_core(p: &MarketPlugin) -> bool {
    is_core_package(&p.id) || p.repo.as_deref().is_some_and(is_core_repo)
}

/// Drops core packages from one source's listing, logging each drop.
fn drop_core_packages(src_id: &str, list: Vec<MarketPlugin>) -> Vec<MarketPlugin> {
    list.into_iter()
        .filter(|p| {
            if entry_is_core(p) {
                crate::log_warn!("插件源「{src_id}」返回核心包「{}」，已丢弃", p.id);
                false
            } else {
                true
            }
        })
        .collect()
}

fn max_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

/// Merges catalogs in source-priority order. A duplicate id keeps the highest
/// credibility tier's identity fields; stars/downloads take the max while
/// attribution, category and the repo hint are unioned/filled in.
fn merge_plugins(collected: Vec<(u32, Vec<MarketPlugin>)>) -> Vec<MarketPlugin> {
    let mut ordered = collected;
    ordered.sort_by_key(|(order, _)| *order);
    let mut out: Vec<MarketPlugin> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (_, list) in ordered {
        for mut mp in list {
            if !mp.sources.contains(&mp.source) {
                mp.sources.insert(0, mp.source.clone());
            }
            match index.get(&mp.id).copied() {
                Some(i) => merge_into(&mut out[i], mp),
                None => {
                    index.insert(mp.id.clone(), out.len());
                    out.push(mp);
                }
            }
        }
    }
    out
}

fn merge_into(dst: &mut MarketPlugin, src: MarketPlugin) {
    for s in std::iter::once(src.source.clone()).chain(src.sources.iter().cloned()) {
        if !dst.sources.contains(&s) {
            dst.sources.push(s);
        }
    }
    dst.stars = max_opt(dst.stars, src.stars);
    dst.downloads = max_opt(dst.downloads, src.downloads);
    if dst.category.is_none() {
        dst.category = src.category;
    }
    if src.confidence > dst.confidence {
        // The more trustworthy source wins wholesale — including its repo hint,
        // which alpha resolution and alpha installs both follow. Keeping a
        // lower-trust source's `repo` would make the installed/queried repo
        // disagree with the entry the user selected.
        dst.confidence = src.confidence;
        dst.source = src.source;
        dst.name = src.name;
        dst.description = src.description;
        dst.urls = src.urls;
        dst.relationship = src.relationship;
        dst.support_versions = src.support_versions;
        dst.verification = src.verification;
        dst.repo = src.repo;
    } else if dst.repo.is_none() {
        // Equal or lower tier: only fill a gap we cannot otherwise resolve.
        dst.repo = src.repo;
    }
}

fn matches_query(p: &MarketPlugin, q: &str) -> bool {
    if p.id.to_lowercase().contains(q) || p.name.to_lowercase().contains(q) {
        return true;
    }
    match &p.description {
        Some(MarketDescription::Plain(s)) => s.to_lowercase().contains(q),
        Some(MarketDescription::Localized(list)) => {
            list.iter().any(|d| d.content.to_lowercase().contains(q))
        }
        None => false,
    }
}

/// Overall budget for one market refresh. Sources are fetched concurrently, so
/// this bounds the whole listing: a source that misses the deadline is skipped
/// (it keeps whatever it cached on a previous run, and is logged), rather than
/// holding the market hostage.
const MARKET_FETCH_BUDGET: std::time::Duration = std::time::Duration::from_secs(45);

/// Core market listing: fetches every enabled source concurrently, drops core
/// packages, merges duplicates, then filters by `query`. Sources that fail or
/// exceed the deadline are skipped in favour of their last-good cache, so one
/// bad source never prevents the rest of the market from rendering.
async fn fetch_market_impl(
    sources: Vec<PluginSourceConfig>,
    cache_dir: Option<std::path::PathBuf>,
    query: Option<String>,
) -> Vec<MarketPlugin> {
    let mut all_enabled: Vec<PluginSourceConfig> =
        sources.into_iter().filter(|s| s.enabled).collect();
    all_enabled.sort_by_key(|s| s.order);

    // Tasks report `(order, id, result)`. An aborted task's output is discarded
    // by JoinSet, so which sources finished is tracked here rather than read
    // back out of the set.
    let mut set: tokio::task::JoinSet<(u32, String, Result<Vec<MarketPlugin>, String>)> =
        tokio::task::JoinSet::new();
    for src in all_enabled.iter().cloned() {
        let dir = cache_dir.clone();
        set.spawn(async move {
            let res = fetch_source(&src, dir.as_deref()).await;
            (src.order, src.id, res)
        });
    }

    let mut collected: Vec<(u32, Vec<MarketPlugin>)> = Vec::new();
    let mut finished: std::collections::HashSet<String> = std::collections::HashSet::new();
    let deadline = tokio::time::Instant::now() + MARKET_FETCH_BUDGET;
    loop {
        let joined = match tokio::time::timeout_at(deadline, set.join_next()).await {
            Ok(Some(j)) => j,
            // No tasks left.
            Ok(None) => break,
            // Budget exhausted: the rest are aborted below.
            Err(_) => break,
        };
        match joined {
            Ok((order, id, Ok(list))) => {
                finished.insert(id.clone());
                collected.push((order, drop_core_packages(&id, list)));
            }
            Ok((_, id, Err(e))) => {
                finished.insert(id.clone());
                crate::log_warn!("插件源「{id}」获取失败，忽略: {e}");
            }
            Err(e) => crate::log_warn!("插件源任务异常: {e}"),
        }
    }
    // Sources that never reported (over budget or panicked): fall back to their
    // last-good cache so they still contribute last-known data.
    set.abort_all();
    for src in all_enabled.iter() {
        if finished.contains(&src.id) {
            continue;
        }
        if let Some(dir) = cache_dir.as_deref() {
            if let Some(c) = read_source_cache(dir, &src.id) {
                crate::log_warn!(
                    "插件源「{}」超出 {}s 预算，改用 last-good 缓存({} 条)",
                    src.id,
                    MARKET_FETCH_BUDGET.as_secs(),
                    c.plugins.len()
                );
                collected.push((src.order, c.plugins));
                continue;
            }
        }
        crate::log_warn!(
            "插件源「{}」超出 {}s 预算且无缓存，本次跳过",
            src.id,
            MARKET_FETCH_BUDGET.as_secs()
        );
    }

    let plugins = merge_plugins(collected);
    let q = query
        .as_deref()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    match q {
        Some(q) => plugins
            .into_iter()
            .filter(|p| matches_query(p, &q))
            .collect(),
        None => plugins,
    }
}

/// The source list the launcher actually uses: the `DSHLAUNCHER_PLUGIN_SOURCES`
/// override when present and parseable, else the persisted settings (with the
/// built-in defaults for configs written before issue #46).
fn effective_plugin_sources(state: &AppState) -> Vec<PluginSourceConfig> {
    if let Ok(raw) = std::env::var(PLUGIN_SOURCES_ENV) {
        let parsed = parse_sources_env(&raw);
        if !parsed.is_empty() {
            return parsed;
        }
        crate::log_warn!("{PLUGIN_SOURCES_ENV} 未解析出有效源，回退到设置");
    }
    let mut list = state.config.lock().unwrap().settings.plugin_sources.clone();
    if list.is_empty() {
        list = default_plugin_sources();
    }
    list.sort_by_key(|s| s.order);
    list
}

fn parse_source_kind(s: &str) -> Option<SourceKind> {
    match s {
        "primary" => Some(SourceKind::Primary),
        "awesome" => Some(SourceKind::Awesome),
        "dsh-get" | "dshget" => Some(SourceKind::DshGet),
        "github-topic" => Some(SourceKind::GithubTopic),
        _ => None,
    }
}

fn infer_source_kind(url: &str) -> SourceKind {
    if url.contains("dsh-plug.in") {
        SourceKind::Primary
    } else if url.contains("awesome-dsh-plugin") {
        SourceKind::Awesome
    } else {
        SourceKind::DshGet
    }
}

fn confidence_for(kind: SourceKind) -> Confidence {
    match kind {
        SourceKind::Primary => Confidence::Official,
        SourceKind::Awesome => Confidence::Curated,
        SourceKind::DshGet => Confidence::Aggregated,
        SourceKind::GithubTopic => Confidence::Unverified,
    }
}

fn derive_source_id(url: &str) -> String {
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("source");
    format!("custom-{}", sanitize_name(host).to_lowercase())
}

/// Parses `DSHLAUNCHER_PLUGIN_SOURCES`: comma-separated entries, each either a
/// bare `url` or `id|kind|url`. Invalid entries are logged and skipped.
fn parse_sources_env(raw: &str) -> Vec<PluginSourceConfig> {
    let mut out = Vec::new();
    for (i, item) in raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .enumerate()
    {
        let parts: Vec<&str> = item.split('|').map(str::trim).collect();
        let (id, kind, url) = match parts.as_slice() {
            [url] => (None, infer_source_kind(url), *url),
            [id, kind, url] => match parse_source_kind(kind) {
                Some(k) => (Some(*id), k, *url),
                None => {
                    crate::log_warn!("忽略未知插件源 kind「{kind}」: {item}");
                    continue;
                }
            },
            _ => {
                crate::log_warn!("忽略格式非法的插件源条目(需 url 或 id|kind|url): {item}");
                continue;
            }
        };
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            crate::log_warn!("忽略非 http(s) 的插件源: {item}");
            continue;
        }
        let id = match id.filter(|s| !s.is_empty()) {
            Some(id) => id.to_string(),
            None => derive_source_id(url),
        };
        out.push(PluginSourceConfig {
            id,
            url: url.to_string(),
            kind,
            enabled: true,
            confidence: confidence_for(kind),
            order: i as u32,
        });
    }
    out
}

/// Lists the configured plugin catalog sources (issue #46); the frontend uses
/// this for the market's source filter and the settings editor.
#[tauri::command]
pub fn list_plugin_sources(state: State<'_, AppState>) -> Result<Vec<PluginSourceConfig>, String> {
    Ok(effective_plugin_sources(state.inner()))
}

/// Fetches the marketplace plugin catalog from every enabled source and merges
/// them. A source that fails to fetch or parse degrades to its last-good cache
/// (logged) and never aborts the whole listing. `query` filters by
/// id/name/description (case-insensitive substring).
#[tauri::command(rename_all = "snake_case")]
pub async fn fetch_plugin_market(
    state: State<'_, AppState>,
    query: Option<String>,
) -> Result<Vec<MarketPlugin>, String> {
    let cache_dir = state.data_dir.join("plugin-cache");
    let sources = effective_plugin_sources(state.inner());
    Ok(fetch_market_impl(sources, Some(cache_dir), query).await)
}

// ---------------------------------------------------------------------------
// Commands: versions per channel
// ---------------------------------------------------------------------------

/// Normalises a caller-supplied repo reference to `owner/repo`: accepts
/// `owner/repo`, `https://github.com/owner/repo[.git]`, and
/// `git@github.com:owner/repo.git`. Returns None for anything else.
fn normalize_repo_ref(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let body = if let Some(rest) = raw
        .strip_prefix("https://github.com/")
        .or_else(|| raw.strip_prefix("http://github.com/"))
        .or_else(|| raw.strip_prefix("git@github.com:"))
    {
        rest
    } else if raw.contains("://") || raw.contains('@') || raw.contains(':') {
        return None;
    } else {
        raw
    };
    let body = body.trim_end_matches(".git").trim_end_matches('/');
    let mut parts = body.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(o), Some(r), None) if !o.is_empty() && !r.is_empty() => Some(format!("{o}/{r}")),
        _ => None,
    }
}

/// Resolves the GitHub repo for a plugin: the caller's hint (market entry's
/// `repo` field or repository URL) wins, then a `github:` plugin id. Returns
/// None when neither yields a repo — nothing else is a reliable source.
fn resolve_repo(plugin_id: &str, repo_hint: Option<&str>) -> Option<String> {
    if let Some(hint) = repo_hint {
        if let Some(repo) = normalize_repo_ref(hint) {
            return Some(repo);
        }
    }
    parse_github_id(plugin_id).map(|(repo, _)| repo)
}

/// Fetches versions for a plugin across the requested channel.
/// - stable/beta read the npm registry dist-tags (latest / next) and fall
///   back to the version list ordered by publish time (all at once).
/// - alpha pages through the GitHub commit history (30 per page); `page` is
///   1-based and defaults to 1. `has_more` tells the UI to lazy-load more.
///
/// `repo` is the market entry's `owner/repo` (or repository URL) hint. Alpha
/// needs it because live/unverified entries are not in any static catalog, so
/// the repo cannot be looked up server-side.
#[tauri::command(rename_all = "snake_case")]
pub async fn fetch_plugin_versions(
    plugin_id: String,
    channel: PluginChannel,
    page: Option<u32>,
    repo: Option<String>,
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
        PluginChannel::Alpha => alpha_commit(&plugin_id, page.unwrap_or(1), repo.as_deref()).await,
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
async fn alpha_commit(
    plugin_id: &str,
    page: u32,
    repo_hint: Option<&str>,
) -> Result<PluginVersionPage, String> {
    // Alpha needs the GitHub repo. For a `github:` id the id itself is
    // authoritative (it also carries the monorepo subpath that must match the
    // commits queried); the frontend's repo hint is only used for ids that
    // cannot name a repo on their own, e.g. live-discovered npm packages.
    let repo = if let Some((repo, _)) = parse_github_id(plugin_id) {
        repo
    } else {
        resolve_repo(plugin_id, repo_hint)
            .ok_or_else(|| format!("插件 {plugin_id} 没有可用的 GitHub 仓库地址"))?
    };

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

/// Parse disabled cordis ids from a profile's cordis.patch.yml.
///
/// A block-aware scan that understands the two shapes a plugin entry can
/// take: a plain top-level `- id:` row (bundle-provided plugins that the user
/// layer merely overrides) and an `- insert:` block whose child `- id:` rows
/// mount first-party/out-of-tree plugins. `disabled: true` can appear on either
/// kind of row, and we collect the id they belong to.
fn read_disabled_ids(dir: &std::path::Path) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    let path = dir.join("cordis.patch.yml");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return set;
    };
    set.extend(disabled_ids(&raw));
    set
}

/// Extract the ids that carry a `disabled: true` row as a *direct child* of
/// their entry.
///
/// Block-aware: a `disabled:` line is attributed to the entry whose `- id:` /
/// `id:` row the scan is inside, at that entry's child indentation. A deeper
/// `disabled:` inside the entry's own `config:` mapping is NOT an entry toggle
/// and is ignored (it would otherwise report a plugin as disabled that the
/// loader still runs).
fn disabled_ids(raw: &str) -> Vec<String> {
    let lines: Vec<&str> = raw.lines().collect();
    let mut out = Vec::new();
    // Current entry's id and its direct-child indentation.
    let mut current_id: Option<String> = None;
    let mut current_child_indent: usize = 0;
    let mut inside_config: Option<usize> = None; // indent of the `config:` key

    for line in &lines {
        let t = line.trim();
        let ind = indent_of(line);

        // A row whose first key is `id` starts/continues an entry.
        if let Some(rest) = t.strip_prefix("- id:") {
            current_id = Some(rest.trim().to_string());
            current_child_indent = ind + 2;
            inside_config = None;
            continue;
        }
        // A bare `id:` key only counts at the top level: a nested `id:` (e.g.
        // `config: { server: { id: fake } }` in an MCP-style config) must NOT
        // reset the current entry, or a deeper `disabled: true` would be
        // attributed to a fabricated id.
        if ind == 0 {
            if let Some(rest) = t.strip_prefix("id:") {
                current_id = Some(rest.trim().to_string());
                current_child_indent = ind + 2;
                inside_config = None;
                continue;
            }
        }

        // A `config:` key opens a nested mapping; anything deeper is not a
        // direct child of the entry.
        if let Some(rest) = t.strip_prefix("config:") {
            if rest.trim().is_empty() || rest.trim() == "true" {
                inside_config = Some(ind);
                continue;
            }
        }

        // A key at or above the `config:` key's own indent is a SIBLING of
        // config (an entry-level key), not its child: close the config block.
        // Without this, an entry-level `disabled: true` written AFTER the
        // config mapping (legal YAML) keeps `inside_config` open forever and
        // is misread as config-internal.
        if let Some(cfg) = inside_config {
            if !t.is_empty() && !t.starts_with('#') && ind <= cfg {
                inside_config = None;
            }
        }

        // A disabled row: only when it is a direct child of the entry (not
        // nested under config) does it gate the entry.
        if let Some(val) = t.strip_prefix("disabled:") {
            let on = val.trim().eq_ignore_ascii_case("true");
            if on && inside_config.is_none() && ind >= current_child_indent && current_id.is_some()
            {
                if let Some(id) = current_id.take() {
                    out.push(id);
                }
                continue;
            }
            continue;
        }

        // Any other list item (a sibling entry, an `- insert:` wrapper, or a
        // deeper nested list) ends the current association.
        if t.starts_with("- ") && !t.starts_with("- id:") && !t.starts_with("- insert") {
            current_id = None;
            inside_config = None;
            continue;
        }
    }
    out
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
// Commands: update check (issue #27)
// ---------------------------------------------------------------------------

/// Update availability for one installed npm plugin.
#[derive(Clone, Debug, Serialize)]
pub struct PluginUpdateInfo {
    pub id: String,
    /// Resolved installed version (from node_modules), or the manifest spec
    /// with range prefixes stripped when the package cannot be read.
    pub current: Option<String>,
    /// Latest stable version (npm dist-tag `latest`); None when the registry
    /// lookup failed or the plugin is not registry-backed.
    pub latest: Option<String>,
    pub has_update: bool,
}

/// Compares two version strings; true when `latest` is strictly newer.
/// Tolerates a leading `v` on either side.
fn semver_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| semver::Version::parse(s.trim().trim_start_matches('v')).ok();
    match (parse(latest), parse(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Strips npm range prefixes (`^`, `~`, `>=`, …) from a manifest spec so it
/// can serve as a fallback "current version" when node_modules is unreadable.
/// Returns None for wildcard / dist-tag specs that carry no version.
fn spec_to_version(spec: &str) -> Option<String> {
    let s = spec
        .trim()
        .trim_start_matches(['^', '~'])
        .trim_start_matches(">=")
        .trim_start_matches("<=")
        .trim_start_matches('=')
        .trim();
    if s.is_empty() || s == "*" || s == "latest" {
        None
    } else {
        Some(s.to_string())
    }
}

/// The installed version of a package in a profile: the real version from
/// `node_modules/<pkg>/package.json`, falling back to the cleaned spec.
fn installed_version_of(dir: &std::path::Path, id: &str, spec: &str) -> Option<String> {
    let pkg_json = dir.join("node_modules").join(id).join("package.json");
    if let Ok(raw) = std::fs::read_to_string(&pkg_json) {
        if let Ok(doc) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(v) = doc.get("version").and_then(|v| v.as_str()) {
                return Some(v.to_string());
            }
        }
    }
    spec_to_version(spec)
}

/// Checks each installed npm plugin in the profile against the registry's
/// `latest` dist-tag (issue #27). Non-registry plugins (git/tgz/local specs)
/// are skipped: their manifest spec does not identify an upstream version.
/// Per-plugin failures degrade to `latest: None` instead of failing the
/// whole check.
#[tauri::command(rename_all = "snake_case")]
pub async fn check_plugin_updates(
    state: State<'_, AppState>,
    instance_id: String,
    profile: String,
) -> Result<Vec<PluginUpdateInfo>, String> {
    let (home_path, _version) = resolve_instance(&state, &instance_id)?;
    let dir = profile_dir(&home_path, &profile);
    let manifest = read_profile_manifest(&dir)?;

    let mut npm_deps: Vec<(String, String)> = Vec::new();
    if let Some(deps) = manifest.get("dependencies").and_then(|d| d.as_object()) {
        for (name, spec) in deps {
            if name.starts_with("@deepseek-ai/") {
                continue;
            }
            let spec = spec.as_str().unwrap_or("");
            // git/tgz/local specs carry no registry version to compare against.
            if spec.contains(':') {
                continue;
            }
            npm_deps.push((name.clone(), spec.to_string()));
        }
    }
    npm_deps.sort();

    let mut set = tokio::task::JoinSet::new();
    for (id, spec) in npm_deps {
        let dir = dir.clone();
        set.spawn(async move {
            let current = installed_version_of(&dir, &id, &spec);
            let latest = match npm_versions(&id, &PluginChannel::Stable).await {
                Ok(versions) => versions
                    .iter()
                    .find(|v| v.is_default)
                    .or(versions.first())
                    .map(|v| v.version.clone()),
                Err(e) => {
                    crate::log_warn!("查询插件 {id} 最新版本失败: {e}");
                    None
                }
            };
            let has_update = match (&current, &latest) {
                (Some(c), Some(l)) => semver_newer(l, c),
                _ => false,
            };
            PluginUpdateInfo {
                id,
                current,
                latest,
                has_update,
            }
        });
    }

    let mut out = Vec::new();
    while let Some(res) = set.join_next().await {
        if let Ok(info) = res {
            out.push(info);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
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

/// Number of leading spaces/tabs of a line.
fn indent_of(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

/// Extract the id from a raw `- id: <id>` / `id: <id>` line's trimmed text.
fn line_id(t: &str) -> Option<&str> {
    if let Some(rest) = t.strip_prefix("- id:") {
        return Some(rest.trim());
    }
    if let Some(rest) = t.strip_prefix("id:") {
        return Some(rest.trim());
    }
    None
}

/// Whether a document holds no real entry (only comments / blank / `[]`).
fn is_doc_empty(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .all(|line| line.is_empty() || line.starts_with('#') || line == "[]")
}

/// Replace the `[]` placeholder of an empty document with nothing (it would be
/// a second YAML document next to real entries) and restore it when the body
/// became empty — the shape ensure_cordis_insert and the toggle rely on.
fn replace_placeholder(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.trim() == "[]" {
            continue;
        }
        out.push(line);
    }
    let joined = out.join("\n");
    let mut result = joined.trim_end().to_string();
    if !result.is_empty() {
        result.push('\n');
    }
    if is_doc_empty(&result) {
        result = "[]\n".to_string();
    }
    result
}

/// A row (line range) of one plugin entry in a cordis.patch.yml document.
///
/// `start`/`end` bound the entry's lines (the `- id:`/`id:` row plus its child
/// keys such as `name:`, `config:`, `disabled:`); `child_indent` is the
/// indentation of those child keys, so a toggled `disabled:` line can be
/// written at the correct level whether the entry sits at the top level or
/// inside an `- insert:` block.
struct EntryRef {
    start: usize,
    end: usize,
    child_indent: usize,
}

impl EntryRef {
    /// Match every row whose id equals one of `ids`. A top-level `- id:` /
    /// `id:` row maps to its whole following block; an `- insert:` block's
    /// child `- id:` rows map to just that child row. Rows nested inside a
    /// `config:` mapping are not top-level entries and are ignored.
    fn find_all(lines: &[&str], ids: &[&str]) -> Vec<EntryRef> {
        let mut out = Vec::new();
        let n = lines.len();
        let is_top = |idx: usize| -> bool {
            let t = lines[idx].trim();
            !t.is_empty() && !t.starts_with('#') && t != "[]" && indent_of(lines[idx]) == 0
        };

        let mut i = 0usize;
        while i < n {
            let t = lines[i].trim();
            let ind = indent_of(lines[i]);
            if ind == 0 && !t.is_empty() && !t.starts_with('#') && t != "[]" {
                if t == "- insert" || t == "- insert:" {
                    // Container end = next top-level entry or EOF.
                    let mut end = i + 1;
                    while end < n && !is_top(end) {
                        end += 1;
                    }
                    let container_child = if i + 1 < n && indent_of(lines[i + 1]) > 0 {
                        indent_of(lines[i + 1])
                    } else {
                        ind + 2
                    };
                    // Child rows inside the container.
                    let mut m = i + 1;
                    while m < end {
                        let tm = lines[m].trim();
                        let indm = indent_of(lines[m]);
                        if (tm.starts_with("- id:") || tm.starts_with("id:"))
                            && indm >= container_child
                        {
                            if let Some(id) = line_id(tm) {
                                if ids.contains(&id) {
                                    let mut child_end = m + 1;
                                    while child_end < end {
                                        let te = lines[child_end].trim();
                                        let inde = indent_of(lines[child_end]);
                                        if !te.is_empty() && !te.starts_with('#') && inde <= indm {
                                            break;
                                        }
                                        child_end += 1;
                                    }
                                    out.push(EntryRef {
                                        start: m,
                                        end: child_end,
                                        child_indent: indm + 2,
                                    });
                                }
                            }
                        }
                        m += 1;
                    }
                    i = end;
                    continue;
                }
                if let Some(id) = line_id(t) {
                    if ids.contains(&id) {
                        let mut end = i + 1;
                        while end < n && !is_top(end) {
                            end += 1;
                        }
                        out.push(EntryRef {
                            start: i,
                            end,
                            child_indent: ind + 2,
                        });
                    }
                }
                // Skip past this top-level block.
                let mut j = i + 1;
                while j < n && !is_top(j) {
                    j += 1;
                }
                i = j;
                continue;
            }
            i += 1;
        }
        out
    }

    /// All entries whose id equals `id` (convenience wrapper).
    fn from_id(lines: &[&str], id: &str) -> Vec<EntryRef> {
        Self::find_all(lines, &[id])
    }

    /// A `BTreeSet` of line indices inside the given entries.
    fn drop_mask(entries: &[EntryRef]) -> std::collections::BTreeSet<usize> {
        let mut set = std::collections::BTreeSet::new();
        for e in entries {
            for idx in e.start..e.end {
                set.insert(idx);
            }
        }
        set
    }
}

/// Strips every cordis.patch.yml block whose id equals `cordis_id` (matching
/// plain `- id:` / `id:` rows, including `- insert:` wrappers) and restores
/// the `[]` placeholder when the document becomes empty.
///
/// Block-aware: removing a top-level `- id:` entry drops its whole child block
/// (its `config:`, `disabled: true`); removing an insert child drops just that
/// child row. Everything else — headers, `!!js` scalars, unrelated entries,
/// blank separators — is preserved byte-for-byte.
fn strip_cordis_rows(raw: &str, cordis_id: &str, plugin_id: &str) -> String {
    let lines: Vec<&str> = raw.lines().collect();
    let targets = EntryRef::find_all(&lines, &[cordis_id, plugin_id]);
    let drop = EntryRef::drop_mask(&targets);
    let kept: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(index, _)| !drop.contains(index))
        .map(|(_, line)| *line)
        .collect();
    let mut result = kept.join("\n");
    if !result.is_empty() {
        result.push('\n');
    }
    if is_doc_empty(&result) {
        result = "[]\n".to_string();
    }
    result
}

/// Add or remove a `disabled: true` row for a cordis id in cordis.patch.yml.
///
/// The edit is block-aware: `disabled: true` is set/cleared on the targeted
/// entry's own row — both a plain top-level `- id:` entry and an `- insert:`
/// block's child `- id:` row are recognized, and the insert wrapper itself is
/// never touched. All other lines (comments, `!!js` scalars, unrelated blocks,
/// blank separators) are preserved byte-for-byte, so the document keeps
/// defeating neither the YAML parser nor the loader's `applyEntryPatches`
/// (which must see the inserted entry before the `disabled` override).
///
/// For a top-level entry that exists in the document (either a plain `- id:`
/// row or an `- insert:` child), the `disabled:` line is toggled on that row.
/// When the id occurs nowhere in the document and `enabled` is false a fresh
/// `- id:` + `disabled: true` override is appended (bundle-provided entries
/// are disabled exactly this way — the loader applies bundle layers first,
/// then this user layer). When `enabled` is true and the id is absent there is
/// nothing to clear, so the document is returned unchanged.
fn set_disabled_row(raw: &str, cordis_id: &str, enabled: bool) -> String {
    let lines: Vec<&str> = raw.lines().collect();
    let targets = EntryRef::from_id(&lines, cordis_id);

    if targets.is_empty() {
        // No row for this id in the document.
        if enabled {
            // Nothing to clear; still route through replace_placeholder so an
            // empty document keeps its `[]` placeholder instead of "\n".
            return replace_placeholder(&(raw.trim_end().to_string() + "\n"));
        }
        // Append a fresh `- id:` + `disabled: true` override for a
        // bundle-provided entry (the loader applies the bundle layer first,
        // then this user layer, so the override hits).
        let mut base = raw.trim_end().to_string();
        if !base.is_empty() {
            base.push('\n');
        }
        base.push_str("- id: ");
        base.push_str(cordis_id);
        base.push_str("\n  disabled: true\n");
        return replace_placeholder(&base);
    }

    // Edit each matched entry. Distinguish a pure `disabled:` gate (id row +
    // only a `disabled:` line, no other keys) from an entry with real content
    // (it also carries `name:` / `config:` and should keep them):
    // - disabling: ensure `disabled: true` (flip a `false`, add one if absent);
    // - enabling a pure gate: drop the block entirely (it exists only to gate);
    // - enabling a content entry: keep it, remove only the `disabled:` line.
    //
    // Edits are absolute line indices into `lines`. A `to_insert` entry anchors
    // at `t.start` (the id row): the `disabled: true` line is emitted right
    // after that row, at the entry's child indentation. A `to_delete` entry is
    // a line to drop.
    let mut to_delete: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    let mut to_insert: Vec<(usize, String)> = Vec::new(); // (anchor idx, line)
    for t in &targets {
        // Direct children of the entry (between its id row and the next
        // entry). `disabled:` may appear among them.
        let start = t.start;
        let end = t.end;

        // Collect the entry's own child keys, ignoring nested `config:`.
        let mut has_disabled = false;
        let mut disabled_on = false;
        let mut has_other = false;
        for (_idx, line) in lines.iter().enumerate().take(end).skip(start + 1) {
            let tl = line.trim();
            if tl.is_empty() || tl.starts_with('#') || indent_of(line) < t.child_indent {
                // Blank / comment / back to a shallower (non-child) level.
                continue;
            }
            if indent_of(line) != t.child_indent {
                // Deeper nesting (e.g. a `disabled:` key inside the plugin's
                // own `config:` mapping — common for MCP server options) is
                // not an entry-level toggle: ignore it entirely. Missing this
                // guard made disabling such a plugin a silent no-op.
                continue;
            }
            if let Some(value) = tl.strip_prefix("disabled:") {
                has_disabled = true;
                disabled_on = value.trim().eq_ignore_ascii_case("true");
            } else {
                has_other = true;
            }
        }

        if enabled {
            if !has_disabled {
                continue;
            }
            if has_other {
                // Keep the entry, drop only its direct disable line.
                for (idx, line) in lines.iter().enumerate().take(end).skip(start + 1) {
                    if indent_of(line) == t.child_indent && line.trim().starts_with("disabled:") {
                        to_delete.insert(idx);
                    }
                }
            } else {
                // Pure gate: remove the whole block.
                for idx in start..end {
                    to_delete.insert(idx);
                }
            }
        } else if !has_disabled {
            // Add `disabled: true` right after the id row.
            to_insert.push((
                start,
                format!("{}disabled: true", " ".repeat(t.child_indent)),
            ));
        } else if !disabled_on {
            // Flip `disabled: false` -> `disabled: true`: drop the old line and
            // add a true one in its place.
            for (idx, line) in lines.iter().enumerate().take(end).skip(start + 1) {
                if indent_of(line) == t.child_indent && line.trim().starts_with("disabled:") {
                    to_delete.insert(idx);
                    to_insert.push((idx, format!("{}disabled: true", " ".repeat(t.child_indent))));
                }
            }
        }
    }

    let mut out: Vec<String> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        out.push(line.to_string());
        // Insertions anchored at this index are emitted right after it.
        for (anchor, text) in to_insert.iter().filter(|(anchor, _)| *anchor == index) {
            let _ = anchor;
            out.push(text.clone());
        }
        if to_delete.contains(&index) {
            // Remove the line we just pushed.
            out.pop();
            // (an insertion anchored here was already emitted above the line,
            // which matches the flip case: old line removed, true line added)
            for (anchor, text) in to_insert.iter().filter(|(anchor, _)| *anchor == index) {
                let _ = anchor;
                if !out.contains(text) {
                    out.push(text.clone());
                }
            }
        }
    }

    let mut result = out.join("\n");
    if !result.is_empty() {
        result.push('\n');
    }
    // A document emptied by enabling a pure gate falls back to the `[]`
    // placeholder so it stays a valid top-level YAML array.
    replace_placeholder(&result)
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
        dedicated_home_name: None,
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
        repo: None,
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
                    // Live/unverified entries have no catalog entry to look up;
                    // the frontend passes the repo hint with the install input.
                    let repo = resolve_repo(&input.plugin_id, input.repo.as_deref())
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
    fn semver_newer_compares_and_tolerates_v_prefix() {
        assert!(semver_newer("1.2.0", "1.0.0"));
        assert!(semver_newer("v1.2.0", "1.0.0"));
        assert!(semver_newer("1.2.0", "v1.0.0"));
        assert!(!semver_newer("1.0.0", "1.0.0"));
        assert!(!semver_newer("1.0.0", "1.2.0"));
        assert!(semver_newer("1.0.0", "1.0.0-beta.1"));
        // Unparseable input (commit shas, tags) never reports an update.
        assert!(!semver_newer("abc1234", "1.0.0"));
        assert!(!semver_newer("1.2.0", "release-x"));
    }

    #[test]
    fn spec_to_version_strips_range_prefixes() {
        assert_eq!(spec_to_version("^1.0.0"), Some("1.0.0".to_string()));
        assert_eq!(spec_to_version("~1.0.0"), Some("1.0.0".to_string()));
        assert_eq!(spec_to_version(">=1.0.0"), Some("1.0.0".to_string()));
        assert_eq!(spec_to_version("1.0.0"), Some("1.0.0".to_string()));
        assert_eq!(spec_to_version("*"), None);
        assert_eq!(spec_to_version("latest"), None);
        assert_eq!(spec_to_version("  "), None);
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
        // Parsers are source-agnostic; the adapter tags source/confidence.
        assert_eq!(mp.source, "dsh-plugins");
        assert_eq!(mp.confidence, Confidence::Unverified);
        assert_eq!(mp.repo.as_deref(), Some("FuRongJun-1999/dsh-memory"));
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

    // -----------------------------------------------------------------------
    // Issue #46: source registry, dedup, credibility, dshget adapter
    // -----------------------------------------------------------------------

    fn src(id: &str, kind: SourceKind, confidence: Confidence, order: u32) -> PluginSourceConfig {
        PluginSourceConfig {
            id: id.to_string(),
            url: format!("https://example.test/{id}.json"),
            kind,
            enabled: true,
            confidence,
            order,
        }
    }

    fn entry(id: &str, name: &str) -> MarketPlugin {
        MarketPlugin {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            support_versions: None,
            urls: None,
            relationship: None,
            source: default_source_id(),
            confidence: Confidence::default(),
            sources: Vec::new(),
            repo: None,
            verification: None,
            category: None,
            stars: None,
            downloads: None,
        }
    }

    #[test]
    fn core_packages_are_never_market_entries() {
        assert!(is_core_package("@deepseek-ai/dsh"));
        assert!(is_core_package("github:deepseek-ai/dsh"));
        assert!(is_core_package("github:DeepSeek-AI/dsh#path:packages/x"));
        assert!(!is_core_package("@dsh-plugin/dsh-loader"));
        assert!(!is_core_package("github:someone/deepseek-ai-tools"));
        assert!(!is_core_package("dsh-approve-for-me"));
    }

    #[test]
    fn core_check_resists_cosmetic_variants() {
        // Leading/trailing whitespace and the `npm:` alias form.
        assert!(is_core_package("  @deepseek-ai/dsh  "));
        assert!(is_core_package("npm:@deepseek-ai/dsh"));
        // A benign-looking install id with a core repo hint is still core: the
        // hint is what alpha resolution and installation follow.
        let mut p = entry("some-innocent-name", "innocent");
        p.repo = Some("deepseek-ai/dsh".to_string());
        assert!(entry_is_core(&p));
        assert_eq!(drop_core_packages("test", vec![p]).len(), 0);
        // A non-core repo hint passes.
        let mut ok = entry("dsh-tool", "tool");
        ok.repo = Some("someone/dsh-tool".to_string());
        assert!(!entry_is_core(&ok));
    }

    #[test]
    fn cache_files_do_not_collide_for_distinct_ids() {
        let dir = std::path::Path::new("plugin-cache");
        assert_ne!(
            cache_file(dir, "a/b"),
            cache_file(dir, "a_b"),
            "distinct source ids must not share a cache file"
        );
        assert_ne!(cache_file(dir, "x"), cache_file(dir, "y"));
        // Path traversal is neutralised: the result never escapes the dir.
        let evil = cache_file(dir, "../../../etc/passwd");
        assert_eq!(evil.parent(), Some(dir));
    }

    #[test]
    fn tag_entry_stamps_source_and_confidence() {
        let mut mp = entry("github:o/r", "r");
        tag_entry(
            &mut mp,
            &src("dshget", SourceKind::DshGet, Confidence::Aggregated, 2),
        );
        assert_eq!(mp.source, "dshget");
        assert_eq!(mp.confidence, Confidence::Aggregated);
        assert_eq!(mp.sources, vec!["dshget"]);
    }

    #[test]
    fn merge_keeps_highest_confidence_and_unions_attribution() {
        let mut low = entry("github:o/r", "from-awesome");
        low.source = "awesome-dsh-plugin".to_string();
        low.sources = vec!["awesome-dsh-plugin".to_string()];
        low.confidence = Confidence::Curated;
        low.stars = Some(10);
        low.category = Some("ui".to_string());

        let mut high = entry("github:o/r", "from-official");
        high.source = "dsh-plugins".to_string();
        high.sources = vec!["dsh-plugins".to_string()];
        high.confidence = Confidence::Official;
        high.stars = Some(3);
        high.repo = Some("o/r".to_string());

        // Higher-order source first (same order the registry uses: official
        // listed before the community catalog): the later, higher tier must
        // still win, including its repo hint.
        let merged = merge_plugins(vec![(1, vec![low.clone()]), (0, vec![high.clone()])]);
        assert_eq!(merged.len(), 1);
        let m = &merged[0];
        assert_eq!(m.source, "dsh-plugins");
        assert_eq!(m.confidence, Confidence::Official);
        assert_eq!(m.name, "from-official");
        // stars take the max across sources, attribution is unioned.
        assert_eq!(m.stars, Some(10));
        assert_eq!(m.category.as_deref(), Some("ui"));
        assert_eq!(m.repo.as_deref(), Some("o/r"));
        assert!(m.sources.contains(&"dsh-plugins".to_string()));
        assert!(m.sources.contains(&"awesome-dsh-plugin".to_string()));

        // Budget 0 (lower order) first, then the better source: the winner's
        // repo hint must replace the low-trust one, not be shadowed by it.
        let mut low_other = low.clone();
        low_other.repo = Some("attacker/mirror".to_string());
        let merged2 = merge_plugins(vec![(0, vec![low_other]), (2, vec![high.clone()])]);
        assert_eq!(merged2.len(), 1);
        assert_eq!(merged2[0].confidence, Confidence::Official);
        assert_eq!(
            merged2[0].repo.as_deref(),
            Some("o/r"),
            "the more trustworthy source's repo hint must win"
        );

        // Equal tiers (different ids): the first-processed entry keeps identity,
        // and a missing repo is filled from the later one.
        let mut first = entry("github:a/x", "first");
        first.source = "s1".to_string();
        first.confidence = Confidence::Aggregated;
        let mut second = entry("github:a/x", "second");
        second.source = "s2".to_string();
        second.confidence = Confidence::Aggregated;
        second.repo = Some("a/x".to_string());
        let merged3 = merge_plugins(vec![(0, vec![first]), (1, vec![second])]);
        assert_eq!(merged3[0].name, "first");
        assert_eq!(merged3[0].source, "s1");
        assert_eq!(merged3[0].repo.as_deref(), Some("a/x"));
    }

    #[test]
    fn dshget_entries_are_parsed_and_uninstallable_skipped() {
        let raw = r#"{
            "plugins": [
                {
                    "name": "dsh-thing",
                    "url": "https://github.com/omdsh-dev/dsh-thing",
                    "category": "ui",
                    "description": { "en": "A thing.", "zh": "一个东西。" },
                    "stars": 12,
                    "install": "dsh plugin --profile web add github:omdsh-dev/dsh-thing",
                    "sources": ["omdsh-hub", "github-topic"],
                    "verification": null,
                    "installable": true
                },
                {
                    "name": "broken",
                    "install": "dsh plugin --profile web add github:omdsh-dev/broken",
                    "installable": false
                },
                {
                    "name": "no-add-line",
                    "install": "not a plugin command",
                    "installable": true
                }
            ]
        }"#;
        let cat: DshGetCatalog = serde_json::from_str(raw).unwrap();
        let parsed: Vec<MarketPlugin> = cat.plugins.iter().filter_map(dshget_to_market).collect();
        assert_eq!(
            parsed.len(),
            1,
            "uninstallable + unparsable entries dropped"
        );
        let mp = &parsed[0];
        assert_eq!(mp.id, "github:omdsh-dev/dsh-thing");
        assert_eq!(mp.repo.as_deref(), Some("omdsh-dev/dsh-thing"));
        assert_eq!(mp.category.as_deref(), Some("ui"));
        assert_eq!(mp.stars, Some(12));
        assert_eq!(mp.sources, vec!["omdsh-hub", "github-topic"]);
        match &mp.description {
            Some(MarketDescription::Localized(list)) => assert_eq!(list.len(), 2),
            other => panic!("expected localized description, got {other:?}"),
        }
    }

    #[test]
    fn normalize_repo_ref_accepts_common_forms() {
        assert_eq!(normalize_repo_ref("o/r").as_deref(), Some("o/r"));
        assert_eq!(
            normalize_repo_ref("https://github.com/o/r.git").as_deref(),
            Some("o/r")
        );
        assert_eq!(
            normalize_repo_ref("git@github.com:o/r.git").as_deref(),
            Some("o/r")
        );
        assert_eq!(normalize_repo_ref("https://gitlab.com/o/r"), None);
        assert_eq!(normalize_repo_ref("github:o/r"), None);
        assert_eq!(normalize_repo_ref(""), None);
    }

    #[test]
    fn resolve_repo_prefers_hint_then_github_id() {
        assert_eq!(
            resolve_repo("some-npm-pkg", Some("https://github.com/o/r")).as_deref(),
            Some("o/r")
        );
        assert_eq!(
            resolve_repo("github:o/r#path:sub", None).as_deref(),
            Some("o/r")
        );
        assert_eq!(resolve_repo("some-npm-pkg", None), None);
    }

    #[test]
    fn parse_sources_env_parses_forms_and_rejects_invalid() {
        let parsed = parse_sources_env(
            "https://mirror.test/catalog.json, my|awesome|https://a.test/p.json, bad|nope|https://b.test/x.json, ftp://c.test/x.json",
        );
        assert_eq!(parsed.len(), 2, "invalid kind and non-http are dropped");
        assert_eq!(parsed[0].kind, SourceKind::DshGet);
        assert_eq!(parsed[0].confidence, Confidence::Aggregated);
        assert!(parsed[0].id.starts_with("custom-"));
        assert_eq!(parsed[1].id, "my");
        assert_eq!(parsed[1].kind, SourceKind::Awesome);
        assert_eq!(parsed[1].confidence, Confidence::Curated);
        assert_eq!(parsed[1].order, 1);
    }

    #[test]
    fn topic_denoise_accepts_plugins_and_rejects_core_and_noise() {
        // Self-identifying by name.
        assert!(topic_entry_accepted("dsh-memory", "someone", false));
        assert!(topic_entry_accepted("dsh_thing", "someone", false));
        assert!(topic_entry_accepted("my-dsh-plugin", "someone", false));
        // Non-obvious name but a real DSH manifest.
        assert!(topic_entry_accepted("harness-extras", "someone", true));
        // Name heuristic AND manifest both absent -> dropped.
        assert!(!topic_entry_accepted("random-large-repo", "someone", false));
        // Core repos are never accepted, manifest or not.
        assert!(!topic_entry_accepted("dsh", "deepseek-ai", true));
        assert!(!topic_entry_accepted("DeepSeek-V3", "deepseek-ai", true));
        // Known catalogs/aggregators are denylisted.
        assert!(!topic_entry_accepted("dshget-data", "bobby-sheng", true));
        assert!(!topic_entry_accepted("dsh-launcher", "dsh-plugins", true));
    }

    #[test]
    fn topic_denied_matches_owner_and_full_name_case_insensitively() {
        assert!(topic_denied("DeepSeek-AI/dsh"));
        assert!(topic_denied("deepseek-ai/anything"));
        assert!(topic_denied("omdsh-dev/dsh-hub-workshop"));
        assert!(!topic_denied("someone/dsh-memory"));
        assert!(!topic_denied("deepseek-ai-fan/dsh-tool"));
    }

    #[test]
    fn core_packages_are_dropped_from_a_source_listing() {
        let list = vec![
            entry("@deepseek-ai/dsh", "core-npm"),
            entry("github:deepseek-ai/dsh", "core-git"),
            entry("github:someone/dsh-tool", "ok"),
        ];
        let kept = drop_core_packages("test", list);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, "github:someone/dsh-tool");
    }

    #[tokio::test]
    async fn unreachable_source_falls_back_to_last_good_cache() {
        let dir = std::env::temp_dir().join(format!("dsh-plugins-cache-{}", uuid::Uuid::new_v4()));
        let mut s = src("dshget", SourceKind::DshGet, Confidence::Aggregated, 2);
        // Port 1 refuses connections immediately, so this fails fast offline.
        s.url = "http://127.0.0.1:1/catalog.json".to_string();
        let cached = vec![entry("github:o/r", "from-cache")];
        write_source_cache(&dir, &s.id, &cached);

        let got = fetch_source(&s, Some(&dir))
            .await
            .expect("an unreachable source must degrade to its last-good cache");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "from-cache");

        // With no cache, the failure is surfaced (the caller logs and skips it).
        let empty_dir = dir.join("empty");
        assert!(fetch_source(&s, Some(&empty_dir)).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_topic_cache_is_served_when_fresh() {
        let dir = std::env::temp_dir().join(format!("dsh-plugins-cache-{}", uuid::Uuid::new_v4()));
        let s = src(
            "github-topic",
            SourceKind::GithubTopic,
            Confidence::Unverified,
            3,
        );
        let cached = vec![entry("github:o/r", "r")];
        write_source_cache(&dir, &s.id, &cached);
        assert!(read_source_cache(&dir, &s.id).is_some());
        // A fresh cache must round-trip the entries verbatim.
        let read = read_source_cache(&dir, &s.id).unwrap();
        assert_eq!(read.plugins.len(), 1);
        assert!(now_ts() - read.saved_at < TOPIC_CACHE_TTL_SECS);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stale_topic_cache_is_refetched_instead_of_served() {
        let dir = std::env::temp_dir().join(format!("dsh-plugins-cache-{}", uuid::Uuid::new_v4()));
        let s = src(
            "github-topic",
            SourceKind::GithubTopic,
            Confidence::Unverified,
            3,
        );
        // Write a cache whose timestamp is past the TTL.
        std::fs::create_dir_all(&dir).unwrap();
        let stale = SourceCache {
            saved_at: now_ts() - TOPIC_CACHE_TTL_SECS - 60,
            plugins: vec![entry("github:stale/x", "stale")],
        };
        std::fs::write(
            cache_file(&dir, &s.id),
            serde_json::to_string(&stale).unwrap(),
        )
        .unwrap();

        // The TTL only short-circuits the network call while the cache is fresh
        // (`now - saved_at < TTL`), so a stale entry necessarily reaches
        // `fetch_catalog`: either the refresh succeeds and rewrites the file, or
        // it fails and the stale payload returns as last-good. Assert exactly
        // that, in both directions, without depending on which one happens here
        // — this test previously asserted the offline outcome unconditionally,
        // which passed on a host with no GitHub route and failed on CI runners.
        // The "degrade to last-good" half is also covered host-independently by
        // `unreachable_source_falls_back_to_last_good_cache` (connection
        // refused on port 1).
        let before = read_source_cache(&dir, &s.id).expect("stale cache written above");
        let fetched_live = match fetch_source(&s, Some(&dir)).await {
            Ok(got) => {
                // The only two ways to get here are the successful refresh and
                // the last-good fallback; the fake id tells them apart.
                let served_stale = got.iter().any(|p| p.id == "github:stale/x");
                assert!(
                    served_stale || !got.is_empty(),
                    "a refresh must either succeed or fall back to the stale cache"
                );
                !served_stale
            }
            Err(_) => false,
        };
        // Reading the cache back is pure I/O, so it pins the contract down
        // without touching the network.
        let after = read_source_cache(&dir, &s.id).expect("cache must remain readable");
        if fetched_live {
            assert!(
                now_ts() - after.saved_at < TOPIC_CACHE_TTL_SECS,
                "a successful refetch must refresh the cache timestamp"
            );
            assert!(
                !after.plugins.iter().any(|p| p.id == "github:stale/x"),
                "a successful refetch must replace the stale entries"
            );
        } else {
            assert_eq!(
                after.saved_at, before.saved_at,
                "a failed refresh must not rewrite the cache file"
            );
            assert!(
                after.plugins.iter().any(|p| p.id == "github:stale/x"),
                "a failed refresh must keep the stale payload as last-good"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
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
    fn set_disabled_row_toggles_inside_insert_block() {
        // The issue #28 shape: a plugin mounted via an `- insert:` block must
        // be disabled on its *own child row* — never by deleting the child's
        // `- id:` line (which collapsed the block and broke the YAML) nor by
        // appending an unreferenced override.
        let raw = "# header\n- insert:\n    - id: modlens\n      name: '@liustack/modlens'\n    - id: dsh-memory-evolve\n      name: dsh-memory-evolve\n      config:\n        reviewEnabled: true\n\n- id: keep\n  config:\n    x: 1\n";
        let off = set_disabled_row(raw, "dsh-memory-evolve", false);
        assert!(
            off.contains("- id: dsh-memory-evolve\n      disabled: true"),
            "disabled must land on the child row: {off}"
        );
        // The insert wrapper and its other children survive untouched.
        assert!(off.contains("- id: modlens"), "sibling child kept: {off}");
        assert!(
            off.contains("name: '@liustack/modlens'"),
            "sibling name kept: {off}"
        );
        assert!(off.contains("reviewEnabled: true"), "config kept: {off}");
        assert!(
            off.contains("- id: keep"),
            "other top-level entry kept: {off}"
        );
        // The block still parses and the id is reported disabled.
        assert!(read_disabled_ids_parse(&off)
            .iter()
            .any(|id| id == "dsh-memory-evolve"));
        // Re-enable restores the original content.
        let on = set_disabled_row(&off, "dsh-memory-evolve", true);
        assert!(!on.contains("disabled"), "disable line removed: {on}");
        assert!(on.contains("- id: dsh-memory-evolve"), "child kept: {on}");
        assert!(on.contains("name: dsh-memory-evolve"), "name kept: {on}");
    }

    #[test]
    fn set_disabled_row_bundle_override_appends_and_removes() {
        // A bundle-provided plugin (no row in this document) is disabled by
        // appending `- id:` + `disabled: true`; re-enabling drops the override
        // back out.
        let raw = "# header\n- insert:\n    - id: modlens\n      name: '@liustack/modlens'\n";
        let off = set_disabled_row(raw, "agent-teams", false);
        assert!(
            off.contains("- id: agent-teams\n  disabled: true"),
            "override appended: {off}"
        );
        assert!(
            off.contains("- id: modlens"),
            "existing content kept: {off}"
        );
        let on = set_disabled_row(&off, "agent-teams", true);
        assert!(!on.contains("agent-teams"), "override removed: {on}");
        assert!(on.contains("modlens"), "existing content kept: {on}");
    }

    #[test]
    fn set_disabled_row_real_profile_shape_round_trip() {
        // The exact shape shipped in a real profile (dsh-tui): comment header,
        // several insert blocks, and a plain override block, each separated by
        // blank lines.
        let raw = "# Your patch layer for this dsh profile, applied after every bundle layer:\n\
# a top-level YAML array of loader patch entries (id-targeted config\n\
# overrides, disables, and insert lists; `!!js` expressions allowed).\n\
- insert:\n    - id: modlens\n      name: '@liustack/modlens'\n\n\
- insert:\n    - id: dsh-memory-evolve\n      name: dsh-memory-evolve\n      config:\n        reviewEnabled: true\n        reviewInterval: 10\n\n\
- insert:\n    - id: dsh-wsl-preset\n      name: '@deepseek-ai/dsh-wsl-preset'\n\n\
- insert:\n    - id: llm-zen\n      name: dsh-zen\n\n\
- id: agent-teams\n  config:\n    stateDir: .agent-teams\n    memberProvider: spawn\n    maxMembers: 20\n";

        // Disabling an insert child keeps the document parseable and disables
        // exactly that plugin.
        let off = set_disabled_row(raw, "dsh-memory-evolve", false);
        assert!(
            off.contains("disabled: true"),
            "insert child disabled: {off}"
        );
        assert!(
            !off.contains("- insert: {"),
            "insert must stay a list, not an object: {off}"
        );
        assert!(off.contains("modlens"), "first insert block kept: {off}");
        assert!(
            off.contains("dsh-wsl-preset"),
            "second insert block kept: {off}"
        );
        assert!(off.contains("agent-teams"), "override block kept: {off}");
        assert!(
            off.contains("stateDir: .agent-teams"),
            "override config kept: {off}"
        );
        assert!(
            off.starts_with("# Your patch layer"),
            "comment header kept: {off}"
        );

        // The disabled override block path: agent-teams exists at top level, so
        // `disabled: true` is added into its existing block, not a new block.
        let off2 = set_disabled_row(raw, "agent-teams", false);
        assert!(
            off2.contains("- id: agent-teams\n  disabled: true\n  config:"),
            "override toggled in place: {off2}"
        );
        assert!(
            off2.contains("stateDir: .agent-teams"),
            "config kept: {off2}"
        );

        // Re-enabling a pure override restores the original command for that id.
        let on2 = set_disabled_row(&off2, "agent-teams", true);
        assert!(!on2.contains("disabled: true"), "re-enabled: {on2}");
        assert!(
            on2.contains("- id: agent-teams\n  config:"),
            "override block restored: {on2}"
        );
    }

    #[test]
    fn read_disabled_ids_ignores_insert_children_unless_disabled() {
        // Only entries that actually carry `disabled: true` are reported; an
        // insert child with a config (even a `disabled`-looking config key) is
        // not, and a child whose name merely contains the id is not either.
        let dir = std::env::temp_dir().join(format!("dsh-plugins-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("cordis.patch.yml"),
            "# header\n- insert:\n    - id: enabled-one\n      name: '@x/enabled-one'\n\n- insert:\n    - id: disabled-in-insert\n      disabled: true\n      name: '@x/disabled-in-insert'\n\n- id: disabled-top\n  disabled: true\n\n- id: not-disabled\n  config:\n    disabled: true\n",
        )
        .unwrap();
        let set = read_disabled_ids(&dir);
        assert!(
            set.contains("disabled-in-insert"),
            "insert child reported: {set:?}"
        );
        assert!(set.contains("disabled-top"), "top-level reported: {set:?}");
        assert!(
            !set.contains("enabled-one"),
            "enabled child not reported: {set:?}"
        );
        assert!(
            !set.contains("not-disabled"),
            "config key not reported: {set:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    fn read_disabled_ids_parse(raw: &str) -> Vec<String> {
        disabled_ids(raw)
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
    async fn live_dshget_catalog_parses_and_filters_core() {
        let dshget = default_plugin_sources()
            .into_iter()
            .find(|s| s.id == "dshget")
            .expect("dshget source is a default");
        let list = fetch_catalog(&dshget)
            .await
            .expect("dshget catalog must be fetchable");
        assert!(list.len() > 1000, "catalog is large, got {}", list.len());
        assert!(
            list.iter().all(|p| p.source == "dshget"),
            "every entry must be stamped with the source id"
        );
        assert!(
            list.iter().all(|p| p.confidence == Confidence::Aggregated),
            "every entry must carry the source's confidence"
        );
        assert!(
            list.iter().all(|p| !is_core_package(&p.id)),
            "the adapter must not emit core packages (checked by fetch_market_impl too)"
        );
        // github: ids dominate this catalog, and every entry must carry a repo
        // hint so alpha version resolution works without a static catalog.
        assert!(
            list.iter()
                .filter(|p| p.id.starts_with("github:"))
                .all(|p| p.repo.is_some()),
            "github: entries need a repo hint"
        );
    }

    /// Market half of the live smoke test: fetches every default source over the
    /// real network. Deliberately does NOT touch `api.github.com` (unreachable on
    /// some networks without a proxy), so this half stays runnable anywhere the
    /// three static catalogs are reachable.
    #[tokio::test]
    #[ignore]
    async fn live_fetch_market() {
        let sources = default_plugin_sources();
        let plugins = fetch_market_impl(sources.clone(), None, None).await;
        assert!(!plugins.is_empty(), "market must return plugins");
        // Every enabled source must have contributed: a source that failed only
        // logs a warning and is skipped, so a missing source id is the signal.
        for src in sources.iter().filter(|s| s.enabled) {
            assert!(
                plugins.iter().any(|p| p.source == src.id),
                "enabled source `{}` contributed nothing",
                src.id
            );
        }
        // The core-package red line holds for the merged live result too.
        for p in &plugins {
            assert!(
                !is_core_package(&p.id),
                "core package leaked into the market: {}",
                p.id
            );
        }
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
    }

    /// Versions half of the live smoke test. Requires npm and `api.github.com`;
    /// on a network without a GitHub proxy the alpha leg cannot run, so this is
    /// a separate test from `live_fetch_market`.
    #[tokio::test]
    #[ignore]
    async fn live_fetch_plugin_versions() {
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
            Some("dsh-plugins/dsh-auxiliary".to_string()),
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
                None,
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

    /// Regression (review): a plugin whose own `config:` mapping carries a
    /// nested `disabled:` key (common for MCP server options) must still be
    /// disable-able — the nested key is not an entry-level toggle, so the
    /// write path must not let it suppress the insert/flip.
    #[test]
    fn set_disabled_row_ignores_disabled_key_inside_config() {
        let raw = "- id: my-plugin\n  config:\n    server:\n      disabled: false\n";
        // Disabling must add the entry-level row despite the nested key.
        let out = set_disabled_row(raw, "my-plugin", false);
        assert!(
            out.contains("- id: my-plugin\n  disabled: true\n"),
            "disable must land on the entry row: {out}"
        );
        assert!(
            out.contains("disabled: false"),
            "nested key preserved: {out}"
        );
        // Re-enabling drops only the entry-level row; the nested key survives.
        let back = set_disabled_row(&out, "my-plugin", true);
        assert_eq!(back, raw, "nested config disabled key must survive: {back}");
    }

    /// Regression (review): an entry-level `disabled: true` written AFTER the
    /// `config:` block (legal YAML, e.g. hand-edited) must be attributed to
    /// the entry, not swallowed as config-internal.
    #[test]
    fn disabled_ids_reads_entry_level_disabled_after_config() {
        let raw = "- id: my-plugin\n  config:\n    a: 1\n  disabled: true\n";
        let out = disabled_ids(raw);
        assert_eq!(out, vec!["my-plugin".to_string()], "out: {out:?}");
    }

    /// Regression (review): a nested `id:` inside `config:` must not reset the
    /// current entry — otherwise a deeper `disabled: true` would fabricate an
    /// id that never existed at the entry level.
    #[test]
    fn disabled_ids_ignores_nested_id_inside_config() {
        let raw = "- id: real\n  config:\n    server:\n      id: fake\n      disabled: true\n";
        let out = disabled_ids(raw);
        assert!(out.is_empty(), "no fabricated ids: {out:?}");
    }

    /// Enabling an id absent from an (empty) document must keep the `[]`
    /// placeholder the rest of the tooling relies on, not return "\n".
    #[test]
    fn set_disabled_row_enable_missing_keeps_placeholder() {
        assert_eq!(set_disabled_row("", "ghost", true), "[]\n");
        assert_eq!(set_disabled_row("[]\n", "ghost", true), "[]\n");
    }
}
