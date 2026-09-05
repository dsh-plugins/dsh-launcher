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
        version_dir.join("node_modules").join("@deepseek-ai").join("dsh").join("package.json")
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
        for (home, profiles) in scan_wsl_homes(&distro) {
            let path = PathBuf::from(home);
            if report.homes.iter().any(|h| {
                h.wsl.as_deref() == Some(distro.as_str()) && paths_equal(&h.path, &path)
            }) {
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
fn scan_wsl_homes(distro: &str) -> Vec<(String, Vec<(String, String)>)> {
    // One call lists homes and their profile dirs together; each output
    // line is either `H<TAB><home>` or `P<TAB><home><TAB><profile>`.
    let script = r#"for h in "$HOME"/.dsh*; do [ -d "$h" ] || continue; echo "H	$h"; for p in "$h"/profiles/*; do [ -d "$p" ] || continue; echo "P	$h	$(basename "$p")"; done; done"#;
    let argv = vec!["sh".to_string(), "-c".to_string(), script.to_string()];
    let out = match tauri::async_runtime::block_on(crate::wsl::wsl_output(distro, &argv)) {
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
    homes.retain(|(_, profiles)| !profiles.is_empty() || true);
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

/// Import result: what was created vs skipped (already known).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ImportReport {
    pub homes_added: usize,
    pub versions_added: usize,
    pub instances_added: usize,
    pub skipped_known: usize,
}

/// Imports the user's selection: registers homes / versions and creates one
/// instance per (home, profile). Idempotent — entries whose path (or
/// instance name) already exists are skipped and counted.
#[tauri::command(rename_all = "snake_case")]
pub async fn import_scanned(
    state: State<'_, AppState>,
    input: ImportScannedInput,
) -> Result<ImportReport, String> {
    let mut cfg = state.config.lock().unwrap().clone();
    let mut report = ImportReport::default();

    for dir in &input.versions {
        let path = PathBuf::from(&dir.dir);
        if !path.is_dir() {
            continue;
        }
        if cfg.versions.iter().any(|v| paths_equal(&v.dir, &path)) {
            report.skipped_known += 1;
            continue;
        }
        let version = read_local_version(&path);
        cfg.versions.push(DshVersion {
            id: crate::config::new_id("ver"),
            version,
            dir: path,
            wsl: None,
        });
        report.versions_added += 1;
    }

    for home in &input.homes {
        let path = PathBuf::from(&home.path);
        if !path.is_dir() && home.wsl.is_none() {
            continue;
        }
        let home_id = match cfg
            .homes
            .iter()
            .find(|h| paths_equal(&h.path, &path) && h.wsl == home.wsl)
        {
            Some(existing) => {
                report.skipped_known += 1;
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
                id
            }
        };
        for profile in &home.profiles {
            // One instance per (home, profile); skip names that exist.
            let inst_name = format!(
                "{}·{}",
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "DSH".to_string()),
                profile
            );
            if cfg
                .instances
                .iter()
                .any(|i| i.home_id == home_id && i.name == inst_name)
            {
                report.skipped_known += 1;
                continue;
            }
            cfg.instances.push(DshInstance {
                id: crate::config::new_id("inst"),
                name: inst_name,
                // Prefer an imported/known version; the instance editor can
                // repoint it. `None` versions cannot launch until chosen.
                version_id: cfg
                    .versions
                    .first()
                    .map(|v| v.id.clone())
                    .unwrap_or_default(),
                home_id: home_id.clone(),
                env_overrides: Default::default(),
                default_profile: Some(profile.clone()),
                last_profile: None,
                icon: None,
                port: None,
            });
            report.instances_added += 1;
        }
    }

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
pub async fn detect_external_running(state: State<'_, AppState>) -> Result<Vec<ExternalStatus>, String> {
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
        let p = Path::new("C:\\Users\\x\\.dsh-dev");
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
}
