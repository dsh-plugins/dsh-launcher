//! Local DSH environment discovery (issue #31, problem 1): the launcher's
//! instance model used to depend entirely on its own install flow, so DSH
//! trees that already existed on the machine (source checkouts, npm trees,
//! extra DSH_HOMEs like `~/.dsh-dev`) were invisible. This module scans for
//! them and imports the ones the user picks:
//!
//! - **Homes**: every `%USERPROFILE%\.dsh*` directory plus the `DSH_HOME`
//!   environment variable target; each home's `profiles/` entries are
//!   enumerated and classified (web / tui / other). On Windows, installed
//!   WSL distros are probed for `~/.dsh*` the same way.
//! - **Versions**: not scanned from the whole filesystem (too invasive);
//!   the wizard lets the user add local version directories which are
//!   validated through the same detection the launcher uses internally
//!   (`is_repo_checkout` / `version_bin`), for both npm layouts and source
//!   checkouts. Unbuilt checkouts are reported as "needs build" instead of
//!   failing the import.
//! - **External running**: instances with a pinned port that answer a TCP
//!   connection but were not started by the launcher are reported as
//!   running externally (see `probe_external_port`).

use crate::config::{paths_equal, DshHome, DshInstance, DshVersion};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tauri::State;

use crate::AppState;

// ---------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------

/// A profile found inside a scanned home.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ScannedProfile {
    pub name: String,
    /// "web" | "tui" | "other" (same classification as `list_profile_infos`).
    pub kind: String,
}

/// A DSH_HOME discovered on the machine.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ScannedHome {
    /// Home path: a Windows path, or a Linux path inside `wsl`.
    pub path: PathBuf,
    /// WSL distro name when the home lives inside a distro.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wsl: Option<String>,
    /// Profiles found under `<home>/profiles`, sorted by name.
    pub profiles: Vec<ScannedProfile>,
    /// A HOME record pointing at this path already exists in the config.
    pub already_known: bool,
}

/// A local version directory validated for import (user-picked).
#[derive(Clone, Debug, serde::Serialize)]
pub struct ScannedVersion {
    pub dir: PathBuf,
    /// Best-effort version string from the tree's package.json
    /// (checkout: `apps/cli`, npm: `node_modules/@deepseek-ai/dsh`).
    pub version: String,
    /// "checkout" (source tree) or "npm" (installed package tree).
    pub layout: String,
    /// The CLI entry (`version_bin`) exists and is non-empty. Unbuilt
    /// checkouts are importable but must be built before launching.
    pub ready: bool,
    /// A VERSION record pointing at this directory already exists.
    pub already_known: bool,
}

/// Full scan report for the import wizard.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ScanReport {
    pub homes: Vec<ScannedHome>,
    /// Present so the wizard can disable adding it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_dsh_home: Option<PathBuf>,
}

/// Directories under `root` whose name starts with `.dsh` (`.dsh`,
/// `.dsh-dev`, …), sorted. Pure helper, unit-tested.
fn dsh_home_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(".dsh") && entry.path().is_dir() {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    out
}

/// Reads the DSH CLI version string from a version directory: source
/// checkouts keep the CLI manifest at `apps/cli/package.json`, npm trees at
/// `node_modules/@deepseek-ai/dsh/package.json`. Falls back to "local".
fn read_local_version(version_dir: &Path) -> String {
    let manifest = if is_checkout(version_dir) {
        version_dir.join("apps").join("cli").join("package.json")
    } else {
        version_dir
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh")
            .join("package.json")
    };
    read_pkg_version(&manifest).unwrap_or_else(|| "local".to_string())
}

fn is_checkout(version_dir: &Path) -> bool {
    version_dir
        .join("apps")
        .join("cli")
        .join("package.json")
        .exists()
}

fn read_pkg_version(manifest: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(manifest).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&raw).ok()?;
    doc.get("version")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Home display name: the folder's own name (`.dsh`, `.dsh-dev`, …),
/// suffixed with the distro for WSL homes.
fn home_display_name(path: &Path, wsl: Option<&str>) -> String {
    let folder = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    match wsl {
        Some(distro) => format!("{folder}（{distro}）"),
        None => folder,
    }
}

/// Scans local DSH environments (homes under `%USERPROFILE%` + `DSH_HOME`
/// env + WSL distros) and reports what could be imported.
#[tauri::command(rename_all = "snake_case")]
pub async fn scan_local_dsh(state: State<'_, AppState>) -> Result<ScanReport, String> {
    let cfg = state.config.lock().unwrap().clone();
    let mut report = ScanReport::default();

    // Windows homes: every ~/.dsh* plus the DSH_HOME env target.
    if let Some(userprofile) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
    {
        let root = PathBuf::from(userprofile);
        for path in dsh_home_dirs(&root) {
            report.homes.push(scan_one_home(&cfg, path, None));
        }
    }
    if let Some(env_home) = std::env::var_os("DSH_HOME") {
        let path = PathBuf::from(env_home);
        if path.is_dir() && !report.homes.iter().any(|h| paths_equal(&h.path, &path)) {
            report.env_dsh_home = Some(path.clone());
            report.homes.push(scan_one_home(&cfg, path, None));
        }
    }

    // WSL homes: probe each installed distro for ~/.dsh* (bounded: distros
    // are few; each probe is one short-lived wsl.exe call).
    #[cfg(windows)]
    for distro in crate::wsl::list_distros() {
        for (home, profiles) in scan_wsl_homes(&distro).await {
            let path = PathBuf::from(home);
            if report
                .homes
                .iter()
                .any(|h| h.wsl.as_deref() == Some(distro.as_str()) && paths_equal(&h.path, &path))
            {
                continue;
            }
            let mut scanned = scan_one_home(&cfg, path, Some(distro.clone()));
            scanned.profiles = profiles
                .into_iter()
                .map(|(name, kind)| ScannedProfile { name, kind })
                .collect();
            report.homes.push(scanned);
        }
    }

    Ok(report)
}

/// Builds a `ScannedHome` (profile enumeration + kind + known flag) for a
/// local (non-WSL) home path.
fn scan_one_home(cfg: &crate::config::Config, path: PathBuf, wsl: Option<String>) -> ScannedHome {
    let profiles = local_profiles(&path)
        .into_iter()
        .map(|(name, kind)| ScannedProfile { name, kind })
        .collect();
    ScannedHome {
        already_known: cfg
            .homes
            .iter()
            .any(|h| paths_equal(&h.path, &path) && h.wsl == wsl),
        path,
        wsl,
        profiles,
    }
}

/// Enumerates and classifies the profiles of a local home (skips the
/// template / node_modules entries, same rules as `list_profiles`).
fn local_profiles(home_path: &Path) -> Vec<(String, String)> {
    let profiles_dir = home_path.join("profiles");
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&profiles_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "node_modules" || name == "__temp__" || !entry.path().is_dir() {
                continue;
            }
            let kind = match crate::process::profile_kind(home_path, &name) {
                crate::process::InstanceKind::Web => "web",
                crate::process::InstanceKind::Tui => "tui",
                crate::process::InstanceKind::Other => "other",
            };
            out.push((name, kind.to_string()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Probes `<distro>` for `~/.dsh*` homes and their profiles. Returns
/// (home_path, [(profile_name, kind)]) pairs; empty when the distro has no
/// DSH homes (or wsl.exe is unavailable).
#[cfg(windows)]
async fn scan_wsl_homes(distro: &str) -> Vec<(String, Vec<(String, String)>)> {
    // One call lists homes and their profile dirs together; each output
    // line is either `H<TAB><home>` or `P<TAB><home><TAB><profile>`.
    let script = r#"for h in "$HOME"/.dsh*; do [ -d "$h" ] || continue; echo "H	$h"; for p in "$h"/profiles/*; do [ -d "$p" ] || continue; echo "P	$h	$(basename "$p")"; done; done"#;
    let argv = vec!["sh".to_string(), "-c".to_string(), script.to_string()];
    // `scan_local_dsh` runs on Tauri's tokio runtime, so `wsl_output` must be
    // awaited rather than driven with `async_runtime::block_on` (which would
    // panic with "Cannot start a runtime from within a runtime" on any WSL
    // machine). This was the only non-`.await` `wsl_output` call in the tree.
    let out = match crate::wsl::wsl_output(distro, &argv).await {
        Ok(out) => out,
        Err(_) => return Vec::new(),
    };

    let mut homes: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for line in out.lines() {
        let mut parts = line.split('\t');
        match (parts.next(), parts.next(), parts.next()) {
            (Some("H"), Some(home), _) => {
                let home = home.trim().to_string();
                if !home.is_empty() && !homes.iter().any(|(h, _)| *h == home) {
                    homes.push((home, Vec::new()));
                }
            }
            (Some("P"), Some(home), Some(profile)) => {
                let home = home.trim();
                let profile = profile.trim();
                if profile == "node_modules" || profile == "__temp__" {
                    continue;
                }
                if let Some(entry) = homes.iter_mut().find(|(h, _)| h == home) {
                    entry.1.push((profile.to_string(), String::new()));
                }
            }
            _ => {}
        }
    }
    // Classify WSL profiles through the \\wsl$ UNC share (same filesystem
    // the web/TUI launch path probes), skipping dirs that vanish there.
    for (home, profiles) in homes.iter_mut() {
        let unc = crate::wsl::unc_path(distro, home);
        profiles.retain(|(name, _)| unc.join("profiles").join(name).exists());
        for (name, kind) in profiles.iter_mut() {
            *kind = match crate::process::profile_kind(&unc, name) {
                crate::process::InstanceKind::Web => "web".to_string(),
                crate::process::InstanceKind::Tui => "tui".to_string(),
                crate::process::InstanceKind::Other => "other".to_string(),
            };
        }
    }
    homes.retain(|(_, profiles)| !profiles.is_empty());
    homes
}

/// Validates a user-picked local version directory for import: detects the
/// layout (checkout vs npm), reads the version string and checks the CLI
/// entry exists ("ready"). Unknown trees are rejected with a message.
#[tauri::command(rename_all = "snake_case")]
pub fn validate_local_version(dir: String) -> Result<ScannedVersion, String> {
    let path = PathBuf::from(&dir);
    if !path.is_dir() {
        return Err("目录不存在".to_string());
    }
    let checkout = is_checkout(&path);
    let npm_manifest = path
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("package.json");
    if !checkout && !npm_manifest.exists() {
        return Err(
            "未识别为 DSH 版本目录（既不是源码 checkout，也没有 node_modules/@deepseek-ai/dsh）"
                .to_string(),
        );
    }
    let version = read_local_version(&path);
    let ready = crate::process::version_bin_ready(&path);
    Ok(ScannedVersion {
        dir: path,
        version,
        layout: if checkout { "checkout" } else { "npm" }.to_string(),
        ready,
        already_known: false, // filled by the wizard against the config
    })
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// Wizard input: which scanned homes / profiles and which version dirs to
/// import.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportScannedInput {
    pub homes: Vec<ImportHomeInput>,
    pub versions: Vec<ImportVersionInput>,
    /// Version directory the user picked as the default for newly created
    /// instances (issue #39, R3). Matched by directory (the frontend only
    /// knows dirs, ids are generated at import time); `None` falls back to
    /// the first known version. When set but not found, the affected
    /// instances are skipped with a reason instead of silently binding the
    /// wrong version.
    #[serde(default)]
    pub preferred_version_dir: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportHomeInput {
    pub path: String,
    #[serde(default)]
    pub wsl: Option<String>,
    /// Profiles to create instances for (web / tui kinds; "other" profiles
    /// are importable as plain instances too, user's choice).
    pub profiles: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportVersionInput {
    pub dir: String,
}

/// One import item's outcome: what was added / skipped / failed and why.
/// The wizard renders these line by line so a "success" toast is never a
/// lie about what happened to each chosen home / version / profile.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ImportItem {
    /// "home" | "version" | "instance".
    pub kind: String,
    /// Home path, version dir, or instance name.
    pub name: String,
    /// "added" | "skipped" | "failed".
    pub status: String,
    /// Human-readable reason for skipped/failed items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Import result: aggregate counters (kept for compatibility) plus a
/// per-item breakdown so the frontend can render exactly what happened.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ImportReport {
    pub homes_added: usize,
    pub versions_added: usize,
    pub instances_added: usize,
    pub skipped_known: usize,
    /// Per-item outcomes, one per processed home / version / profile.
    #[serde(default)]
    pub items: Vec<ImportItem>,
}

/// Imports the user's selection into `cfg`: registers homes / versions and
/// creates one instance per (home, profile). Idempotent — entries whose path
/// (or instance name) already exists are skipped and counted. Every entry is
/// reported per-item (`ImportReport.items`); whole-operation failures (e.g.
/// save_state) are surfaced by the caller as `Err`.
fn apply_import(cfg: &mut crate::config::Config, input: &ImportScannedInput) -> ImportReport {
    let mut report = ImportReport::default();

    // Resolve the user-picked default version dir, if any.
    let preferred_dir = input.preferred_version_dir.as_deref().map(PathBuf::from);

    for dir in &input.versions {
        let path = PathBuf::from(&dir.dir);
        if !path.is_dir() {
            report.items.push(ImportItem {
                kind: "version".to_string(),
                status: "failed".to_string(),
                name: dir.dir.clone(),
                reason: Some("目录不存在".to_string()),
            });
            continue;
        }
        if cfg.versions.iter().any(|v| paths_equal(&v.dir, &path)) {
            report.skipped_known += 1;
            report.items.push(ImportItem {
                kind: "version".to_string(),
                status: "skipped".to_string(),
                name: dir.dir.clone(),
                reason: Some("已登记".to_string()),
            });
            continue;
        }
        let version = read_local_version(&path);
        cfg.versions.push(DshVersion {
            id: crate::config::new_id("ver"),
            version,
            dir: path.clone(),
            wsl: None,
        });
        report.versions_added += 1;
        // A version alone is not an instance (issue #39, problem 1): register
        // it but say so explicitly so the user knows why no instance appeared.
        report.items.push(ImportItem {
            kind: "version".to_string(),
            status: "added".to_string(),
            name: dir.dir.clone(),
            reason: None,
        });
    }

    // The version newly created instances should bind to (issue #39, R3):
    // - user explicitly picked one (`preferred_version_dir`) and it is now in
    //   the config -> that version (strict: when picked but NOT found, the
    //   instance is not created and the item is failed instead of silently
    //   binding the wrong version);
    // - no pick -> first known version if any, else none (instance is
    //   created but cannot launch until a version is chosen in the editor).
    let preferred_version_id = preferred_dir.as_ref().and_then(|dir| {
        cfg.versions
            .iter()
            .find(|v| paths_equal(&v.dir, dir))
            .map(|v| v.id.clone())
    });
    let picked_but_missing = preferred_dir.is_some() && preferred_version_id.is_none();
    let fallback_version_id = cfg.versions.first().map(|v| v.id.clone());

    for home in &input.homes {
        let path = PathBuf::from(&home.path);
        if !path.is_dir() && home.wsl.is_none() {
            report.items.push(ImportItem {
                kind: "home".to_string(),
                status: "failed".to_string(),
                name: home.path.clone(),
                reason: Some("目录不存在".to_string()),
            });
            continue;
        }
        let home_id = match cfg
            .homes
            .iter()
            .find(|h| paths_equal(&h.path, &path) && h.wsl == home.wsl)
        {
            Some(existing) => {
                report.skipped_known += 1;
                report.items.push(ImportItem {
                    kind: "home".to_string(),
                    status: "skipped".to_string(),
                    name: home.path.clone(),
                    reason: Some("已登记".to_string()),
                });
                existing.id.clone()
            }
            None => {
                let id = crate::config::new_id("home");
                cfg.homes.push(DshHome {
                    id: id.clone(),
                    name: home_display_name(&path, home.wsl.as_deref()),
                    path: path.clone(),
                    wsl: home.wsl.clone(),
                });
                report.homes_added += 1;
                report.items.push(ImportItem {
                    kind: "home".to_string(),
                    status: "added".to_string(),
                    name: home.path.clone(),
                    reason: None,
                });
                id
            }
        };
        let version_bound = match preferred_version_id.clone() {
            Some(id) => Some(id),
            None if !picked_but_missing => fallback_version_id.clone(),
            // Picked version dir was provided but did not end up in the
            // config (e.g. its registration failed): do not silently bind
            // another version — fail the instance instead.
            None => None,
        };
        let no_version_reason = if picked_but_missing {
            Some("所选版本不可用，实例未创建".to_string())
        } else {
            None
        };
        for profile in &home.profiles {
            // One instance per (home, profile); skip names that exist. The
            // name embeds the home display name (which appends the distro for
            // WSL homes) so a Windows `~/.dsh` and a WSL `~/.dsh` with the
            // same profile no longer produce the same globally-unique name
            // (the editor enforces global name uniqueness in create/rename).
            let inst_name = format!(
                "{}·{}",
                home_display_name(&path, home.wsl.as_deref()),
                profile
            );
            let exists = cfg
                .instances
                .iter()
                .any(|i| i.home_id == home_id && i.name == inst_name);
            if exists {
                report.skipped_known += 1;
                report.items.push(ImportItem {
                    kind: "instance".to_string(),
                    status: "skipped".to_string(),
                    name: inst_name.clone(),
                    reason: Some("同名实例已存在".to_string()),
                });
                continue;
            }
            // The user picked a version that could not be registered: skip
            // creating the instance and report the failure per-item.
            if let Some(reason) = &no_version_reason {
                report.items.push(ImportItem {
                    kind: "instance".to_string(),
                    status: "failed".to_string(),
                    name: inst_name.clone(),
                    reason: Some(reason.clone()),
                });
                continue;
            }
            let version_id = match &version_bound {
                Some(id) => id.clone(),
                None => {
                    // No version at all was provided/known — create the
                    // instance anyway (user can pick a version in the
                    // editor) but tell them clearly.
                    report.items.push(ImportItem {
                        kind: "instance".to_string(),
                        status: "added".to_string(),
                        name: inst_name.clone(),
                        reason: Some("未绑定版本，请在实例编辑器中指定".to_string()),
                    });
                    cfg.instances.push(DshInstance {
                        id: crate::config::new_id("inst"),
                        name: inst_name.clone(),
                        version_id: String::new(),
                        home_id: home_id.clone(),
                        env_overrides: Default::default(),
                        default_profile: Some(profile.clone()),
                        last_profile: None,
                        icon: None,
                        port: None,
                    });
                    report.instances_added += 1;
                    continue;
                }
            };
            cfg.instances.push(DshInstance {
                id: crate::config::new_id("inst"),
                name: inst_name.clone(),
                version_id,
                home_id: home_id.clone(),
                env_overrides: Default::default(),
                default_profile: Some(profile.clone()),
                last_profile: None,
                icon: None,
                port: None,
            });
            report.instances_added += 1;
            report.items.push(ImportItem {
                kind: "instance".to_string(),
                status: "added".to_string(),
                name: inst_name.clone(),
                reason: None,
            });
        }
    }

    report
}

/// Tauri command wrapper around [`apply_import`]: clones the config, applies
/// the import, persists it, and returns the per-item report.
#[tauri::command(rename_all = "snake_case")]
pub async fn import_scanned(
    state: State<'_, AppState>,
    input: ImportScannedInput,
) -> Result<ImportReport, String> {
    let mut cfg = state.config.lock().unwrap().clone();
    let report = apply_import(&mut cfg, &input);
    crate::commands::save_state(&state, &cfg)?;
    Ok(report)
}

/// True when `port` on 127.0.0.1 answers a TCP connect within the timeout.
/// Used to mark launcher-external running instances (issue #31, expectation
/// 2). Best-effort: a refused/timed-out connect means "not detected".
pub async fn probe_external_port(port: u16) -> bool {
    use tokio::net::TcpStream;
    use tokio::time::{timeout, Duration};
    timeout(Duration::from_millis(250), async {
        TcpStream::connect(("127.0.0.1", port)).await.is_ok()
    })
    .await
    .unwrap_or(false)
}

/// Instances running outside the launcher: pinned-port instances whose port
/// answers a TCP connect but which are not tracked in `state.running`
/// (started e.g. via `start-fixed.ps1`). Emitted as Running statuses with
/// `external: true` so the UI can label them without mixing them into the
/// launcher's own lifecycle (no stop / open actions).
#[derive(Clone, Debug, serde::Serialize)]
pub struct ExternalStatus {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub profile: Option<String>,
}

/// Detects launcher-external running instances among the pinned-port ones.
#[tauri::command(rename_all = "snake_case")]
pub async fn detect_external_running(
    state: State<'_, AppState>,
) -> Result<Vec<ExternalStatus>, String> {
    let cfg = state.config.lock().unwrap().clone();
    let mut out = Vec::new();
    for inst in &cfg.instances {
        let Some(port) = inst.port else { continue };
        if state.running.lock().await.contains_key(&inst.id) {
            continue;
        }
        if state.tui_sessions.lock().await.contains_key(&inst.id) {
            continue;
        }
        if probe_external_port(port).await {
            out.push(ExternalStatus {
                id: inst.id.clone(),
                name: inst.name.clone(),
                port,
                profile: inst.last_profile.clone(),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dsh_home_dirs_finds_dsh_prefixed_dirs() {
        let root = std::env::temp_dir().join(format!("dsh-scan-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".dsh")).unwrap();
        std::fs::create_dir_all(root.join(".dsh-dev")).unwrap();
        std::fs::create_dir_all(root.join(".dshx")).unwrap();
        std::fs::create_dir_all(root.join("unrelated")).unwrap();
        // A file named .dsh-must be ignored (dirs only).
        std::fs::write(root.join(".dshfile"), "").unwrap();
        let found = dsh_home_dirs(&root);
        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec![".dsh", ".dsh-dev", ".dshx"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn read_local_version_prefers_cli_manifest_and_falls_back() {
        let dir = std::env::temp_dir().join(format!("dsh-scan-{}", uuid::Uuid::new_v4()));
        // Checkout layout.
        let cli = dir.join("apps").join("cli");
        std::fs::create_dir_all(&cli).unwrap();
        std::fs::write(
            cli.join("package.json"),
            r#"{"name":"@deepseek-ai/dsh-cli","version":"9.9.9-test"}"#,
        )
        .unwrap();
        assert_eq!(read_local_version(&dir), "9.9.9-test");
        // No manifest anywhere -> "local".
        let empty = std::env::temp_dir().join(format!("dsh-scan-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(read_local_version(&empty), "local");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&empty).ok();
    }

    #[test]
    fn home_display_name_appends_distro() {
        // Forward slashes: `Path::file_name` must split the last component on
        // every CI platform (backslash is a separator only on Windows).
        let p = Path::new("C:/Users/x/.dsh-dev");
        assert_eq!(home_display_name(p, None), ".dsh-dev");
        assert_eq!(home_display_name(p, Some("Ubuntu")), ".dsh-dev（Ubuntu）");
    }

    #[cfg(windows)]
    #[test]
    fn validate_rejects_unknown_dir() {
        let dir = std::env::temp_dir().join(format!("dsh-scan-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(super::validate_local_version(dir.to_string_lossy().to_string()).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- apply_import (issue #39) ------------------------------------------

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("dsh-imp-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn import_versions_only_never_creates_instances_and_reports_items() {
        let mut cfg = crate::config::Config::default();
        let vdir = temp_dir("ver");
        let input = ImportScannedInput {
            homes: vec![],
            versions: vec![ImportVersionInput {
                dir: vdir.to_string_lossy().to_string(),
            }],
            preferred_version_dir: None,
        };
        let report = apply_import(&mut cfg, &input);
        // A version alone is not an instance (issue #39, problem 1).
        assert_eq!(report.instances_added, 0);
        assert_eq!(report.versions_added, 1);
        assert_eq!(cfg.versions.len(), 1);
        let version_item = report
            .items
            .iter()
            .find(|i| i.kind == "version")
            .expect("version item reported");
        assert_eq!(version_item.status, "added");
        std::fs::remove_dir_all(&vdir).ok();
    }

    #[test]
    fn import_missing_dirs_fail_per_item_instead_of_silently_skipping() {
        let mut cfg = crate::config::Config::default();
        let missing_version =
            std::env::temp_dir().join(format!("dsh-imp-nope-{}", uuid::Uuid::new_v4()));
        let missing_home =
            std::env::temp_dir().join(format!("dsh-imp-nohome-{}", uuid::Uuid::new_v4()));
        let input = ImportScannedInput {
            homes: vec![ImportHomeInput {
                path: missing_home.to_string_lossy().to_string(),
                wsl: None,
                profiles: vec!["web".to_string()],
            }],
            versions: vec![ImportVersionInput {
                dir: missing_version.to_string_lossy().to_string(),
            }],
            preferred_version_dir: None,
        };
        let report = apply_import(&mut cfg, &input);
        assert_eq!(report.versions_added, 0);
        assert_eq!(report.homes_added, 0);
        assert_eq!(report.instances_added, 0);
        assert!(report
            .items
            .iter()
            .any(|i| i.kind == "version" && i.status == "failed" && i.reason.is_some()));
        assert!(report
            .items
            .iter()
            .any(|i| i.kind == "home" && i.status == "failed" && i.reason.is_some()));
    }

    #[test]
    fn import_creates_instances_and_binds_preferred_version() {
        let mut cfg = crate::config::Config::default();
        let vdir = temp_dir("ver");
        let hdir = temp_dir("home");
        let input = ImportScannedInput {
            homes: vec![ImportHomeInput {
                path: hdir.to_string_lossy().to_string(),
                wsl: None,
                profiles: vec!["web".to_string(), "tui".to_string()],
            }],
            versions: vec![ImportVersionInput {
                dir: vdir.to_string_lossy().to_string(),
            }],
            preferred_version_dir: Some(vdir.to_string_lossy().to_string()),
        };
        let report = apply_import(&mut cfg, &input);
        assert_eq!(report.homes_added, 1);
        assert_eq!(report.versions_added, 1);
        assert_eq!(report.instances_added, 2);
        let version_id = cfg.versions[0].id.clone();
        assert_eq!(cfg.instances.len(), 2);
        assert!(cfg.instances.iter().all(|i| i.version_id == version_id));
        // Every created instance is reported.
        let inst_items: Vec<_> = report
            .items
            .iter()
            .filter(|i| i.kind == "instance")
            .collect();
        assert_eq!(inst_items.len(), 2);
        assert!(inst_items.iter().all(|i| i.status == "added"));
        std::fs::remove_dir_all(&vdir).ok();
        std::fs::remove_dir_all(&hdir).ok();
    }

    #[test]
    fn import_preferred_version_missing_fails_instances() {
        let mut cfg = crate::config::Config::default();
        let hdir = temp_dir("home");
        let picked_that_never_registered =
            std::env::temp_dir().join(format!("dsh-imp-missing-{}", uuid::Uuid::new_v4()));
        let input = ImportScannedInput {
            homes: vec![ImportHomeInput {
                path: hdir.to_string_lossy().to_string(),
                wsl: None,
                profiles: vec!["web".to_string()],
            }],
            versions: vec![],
            // The user picked a version dir that is not among the imported
            // versions and not in the config: no silent wrong binding.
            preferred_version_dir: Some(picked_that_never_registered.to_string_lossy().to_string()),
        };
        let report = apply_import(&mut cfg, &input);
        assert_eq!(report.instances_added, 0);
        assert!(report
            .items
            .iter()
            .any(|i| i.kind == "instance" && i.status == "failed"));
    }

    #[test]
    fn import_is_idempotent_and_counts_skips() {
        let mut cfg = crate::config::Config::default();
        let vdir = temp_dir("ver");
        let hdir = temp_dir("home");
        let make_input = || ImportScannedInput {
            homes: vec![ImportHomeInput {
                path: hdir.to_string_lossy().to_string(),
                wsl: None,
                profiles: vec!["web".to_string()],
            }],
            versions: vec![ImportVersionInput {
                dir: vdir.to_string_lossy().to_string(),
            }],
            preferred_version_dir: Some(vdir.to_string_lossy().to_string()),
        };
        let first = apply_import(&mut cfg, &make_input());
        assert_eq!(first.instances_added, 1);
        assert_eq!(cfg.instances.len(), 1);
        // Second run: everything already known -> skipped, no duplicates.
        let second = apply_import(&mut cfg, &make_input());
        assert_eq!(second.homes_added, 0);
        assert_eq!(second.versions_added, 0);
        assert_eq!(second.instances_added, 0);
        assert_eq!(cfg.instances.len(), 1);
        assert_eq!(second.skipped_known, 3);
        assert!(second
            .items
            .iter()
            .all(|i| i.status == "skipped" || i.status == "added"));
        std::fs::remove_dir_all(&vdir).ok();
        std::fs::remove_dir_all(&hdir).ok();
    }

    #[test]
    fn import_without_any_version_creates_instance_with_warning() {
        let mut cfg = crate::config::Config::default();
        let hdir = temp_dir("home");
        let input = ImportScannedInput {
            homes: vec![ImportHomeInput {
                path: hdir.to_string_lossy().to_string(),
                wsl: None,
                profiles: vec!["web".to_string()],
            }],
            versions: vec![],
            preferred_version_dir: None,
        };
        let report = apply_import(&mut cfg, &input);
        assert_eq!(report.instances_added, 1);
        assert!(cfg.instances[0].version_id.is_empty());
        let item = report
            .items
            .iter()
            .find(|i| i.kind == "instance")
            .expect("instance item");
        assert_eq!(item.status, "added");
        assert!(item.reason.is_some(), "warns that no version was bound");
        std::fs::remove_dir_all(&hdir).ok();
    }
}
