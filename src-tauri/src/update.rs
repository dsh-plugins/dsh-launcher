//! Launcher self-update check against GitHub releases.
//!
//! Uses the GitHub REST API (`/repos/{repo}/releases`) with the anonymous
//! OAuth client id boost (see `plugins::GITHUB_CLIENT_ID`): plain
//! unauthenticated API calls are limited to 60 req/h per IP, and the
//! `releases.atom` feed served from the main github.com domain is not the API
//! at all — under heavy IP load it can be refused at the transport layer
//! (`error sending request for url ...`). Passing the public client id raises
//! the unauth quota to 5000 req/h, exactly as the plugin marketplace does.
//!
//! Channel is chosen by the user in the launcher settings: the `release`
//! channel only considers releases without a semver pre-release segment,
//! while `dev` (default) also looks at prereleases and treats any published
//! stable newer than the running build as an update.

use serde::Deserialize;
use serde::Serialize;

const RELEASES_API: &str = "/repos/dsh-plugins/dsh-launcher/releases?per_page=100";
const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct LauncherUpdateInfo {
    pub current: String,
    /// "dev" when the running build is a -dev.N prerelease, else "stable".
    pub channel: String,
    pub up_to_date: bool,
    pub latest: Option<String>,
    pub url: Option<String>,
    pub published_at: Option<String>,
}

#[derive(Clone, Debug)]
struct ReleaseEntry {
    tag_name: String,
    html_url: String,
    published_at: Option<String>,
}

/// Parses a release tag ("v0.2.0" / "v0.2.0-dev.12") as semver.
fn parse_tag(tag: &str) -> Option<semver::Version> {
    semver::Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

/// Minimal shape of a GitHub REST `/releases` list item.
#[derive(Clone, Debug, Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    published_at: Option<String>,
}

/// Normalises API items into `ReleaseEntry`es, newest first (the API returns
/// releases in reverse-chronological order, but order must not be relied on).
fn parse_releases_json(body: &[u8]) -> Result<Vec<ReleaseEntry>, String> {
    let api: Vec<ApiRelease> =
        serde_json::from_slice(body).map_err(|e| format!("解析 GitHub Releases JSON 失败: {e}"))?;
    let mut out: Vec<ReleaseEntry> = api
        .into_iter()
        .map(|r| ReleaseEntry {
            tag_name: r.tag_name,
            html_url: r.html_url,
            published_at: r.published_at,
        })
        .collect();
    out.sort_by(|a, b| b.tag_name.cmp(&a.tag_name));
    Ok(out)
}

/// Picks the newest published release strictly newer than `current`.
/// `dev_channel` sees prereleases and stables; the release channel sees only
/// stables (pre-releases are ignored).
fn pick_latest<'a>(
    current: &semver::Version,
    dev_channel: bool,
    releases: &'a [ReleaseEntry],
) -> Option<&'a ReleaseEntry> {
    releases
        .iter()
        .filter_map(|r| parse_tag(&r.tag_name).map(|v| (v, r)))
        .filter(|(v, _)| dev_channel || v.pre.is_empty())
        .filter(|(v, _)| v > current)
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, r)| r)
}

/// The update-check channel: "dev" (default) also sees prereleases,
/// "release" only considers stable releases.
fn parse_channel(raw: Option<&str>) -> Result<bool, String> {
    match raw.unwrap_or("dev").trim().to_ascii_lowercase().as_str() {
        "dev" => Ok(true),
        "release" => Ok(false),
        other => Err(format!("无效的更新渠道: {other}")),
    }
}

#[tauri::command]
pub async fn check_launcher_update(channel: Option<String>) -> Result<LauncherUpdateInfo, String> {
    let current_raw = env!("CARGO_PKG_VERSION");
    let current = semver::Version::parse(current_raw)
        .map_err(|e| format!("当前版本号无效 {current_raw}: {e}"))?;
    let dev_channel = parse_channel(channel.as_deref())?;

    let url = crate::plugins::github_api_url(RELEASES_API);
    let client = crate::proxy::apply(reqwest::Client::builder())?
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("dsh-launcher")
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    crate::log_debug!(
        "检查启动器更新（{channel} 渠道）：{url}",
        channel = if dev_channel { "dev" } else { "release" }
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("请求 GitHub Releases 失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GitHub Releases 返回 HTTP {}", resp.status()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取响应失败: {e}"))?;
    if bytes.len() > MAX_BYTES {
        return Err("GitHub Releases 响应过大".to_string());
    }
    let releases = parse_releases_json(&bytes)?;
    crate::log_debug!("GitHub Releases API 解析出 {} 条发布记录", releases.len());

    let latest = pick_latest(&current, dev_channel, &releases);
    match latest {
        Some(r) => crate::log_info!(
            "发现新版本 {}（当前 {current_raw}，{dev} 渠道）",
            r.tag_name,
            dev = if dev_channel { "dev" } else { "release" }
        ),
        None => crate::log_info!("启动器已是最新（{current_raw}）"),
    }
    Ok(LauncherUpdateInfo {
        current: current_raw.to_string(),
        channel: if dev_channel { "dev" } else { "stable" }.to_string(),
        up_to_date: latest.is_none(),
        latest: latest.map(|r| r.tag_name.trim_start_matches('v').to_string()),
        url: latest.map(|r| r.html_url.clone()),
        published_at: latest.and_then(|r| r.published_at.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str) -> ReleaseEntry {
        ReleaseEntry {
            tag_name: tag.to_string(),
            html_url: format!("https://example.com/releases/tag/{tag}"),
            published_at: None,
        }
    }

    fn tag_of(found: Option<&ReleaseEntry>) -> Option<String> {
        found.map(|r| r.tag_name.clone())
    }

    #[test]
    fn json_feed_extracts_tag_link_and_date() {
        let body = br#"[
          {
            "tag_name": "v0.2.0-dev.41",
            "html_url": "https://github.com/dsh-plugins/dsh-launcher/releases/tag/v0.2.0-dev.41",
            "published_at": "2026-08-27T03:54:57Z"
          },
          {
            "tag_name": "v0.2.0-dev.40",
            "html_url": "https://github.com/dsh-plugins/dsh-launcher/releases/tag/v0.2.0-dev.40",
            "published_at": null
          }
        ]"#;
        let entries = parse_releases_json(body).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].tag_name, "v0.2.0-dev.41");
        assert_eq!(
            entries[0].html_url,
            "https://github.com/dsh-plugins/dsh-launcher/releases/tag/v0.2.0-dev.41"
        );
        assert_eq!(
            entries[0].published_at.as_deref(),
            Some("2026-08-27T03:54:57Z")
        );
        assert_eq!(entries[1].published_at, None);
    }

    #[test]
    fn json_feed_rejects_malformed_body() {
        assert!(parse_releases_json(b"not json").is_err());
    }

    #[test]
    fn stable_channel_ignores_prereleases() {
        let current = semver::Version::parse("0.1.9").unwrap();
        let releases = vec![release("v0.2.0-dev.7"), release("v0.1.9")];
        // Only prereleases are newer: a stable build stays put.
        assert_eq!(tag_of(pick_latest(&current, false, &releases)), None);
    }

    #[test]
    fn stable_channel_finds_newer_stable() {
        let current = semver::Version::parse("0.1.9").unwrap();
        let releases = vec![release("v0.2.0-dev.7"), release("v0.2.0")];
        assert_eq!(
            tag_of(pick_latest(&current, false, &releases)),
            Some("v0.2.0".to_string())
        );
    }

    #[test]
    fn parse_channel_accepts_dev_and_release() {
        assert!(parse_channel(None).unwrap());
        assert!(parse_channel(Some("dev")).unwrap());
        assert!(parse_channel(Some("DEV")).unwrap());
        assert!(!parse_channel(Some("release")).unwrap());
        assert!(parse_channel(Some("stable")).is_err());
    }

    #[test]
    fn dev_channel_finds_newer_dev_build() {
        let current = semver::Version::parse("0.2.0-dev.3").unwrap();
        let releases = vec![release("v0.2.0-dev.2"), release("v0.2.0-dev.5")];
        assert_eq!(
            tag_of(pick_latest(&current, true, &releases)),
            Some("v0.2.0-dev.5".to_string())
        );
    }

    #[test]
    fn dev_channel_prefers_shipped_stable_over_dev_build() {
        let current = semver::Version::parse("0.2.0-dev.9").unwrap();
        let releases = vec![release("v0.2.0")];
        assert_eq!(
            tag_of(pick_latest(&current, true, &releases)),
            Some("v0.2.0".to_string())
        );
    }

    #[test]
    fn same_version_and_unparsable_tags_are_excluded() {
        let current = semver::Version::parse("0.2.0-dev.9").unwrap();
        let releases = vec![
            release("v0.2.0-dev.9"), // same as current
            release("garbage-tag"),  // unparsable
        ];
        assert_eq!(tag_of(pick_latest(&current, true, &releases)), None);
    }
}
