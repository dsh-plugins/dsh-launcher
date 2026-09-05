//! Dependency-tree preflight for an instance + profile.
//!
//! DSH's core packages are provided by the CLI's own dependency tree; a
//! profile must never carry its own copy. Two states are therefore treated as
//! faults:
//!
//! 1. the profile's `node_modules` contains any `@deepseek-ai/*` core package;
//! 2. two generations of core are mixed (the copy inside the profile has a
//!    different version than the same package inside the CLI tree).
//!
//! (2) is the silent failure mode: two copies of a core package mint two
//! unequal module-local `Symbol()`s, the agent loop's scheduler lookup on
//! ToolRuntime returns `undefined`, and every tool call in that profile dies
//! in `.prepare` with no load-time error and no hint about which package was
//! duplicated. A launcher that creates many instances/versions is the most
//! likely producer of that state, so it checks for it before starting.
//!
//! The comparator is per package, not the `@deepseek-ai/dsh` generation
//! number (issue #32): the scope mixes generation-following packages (`dsh`,
//! `dsh-settings`, …, all versioned with the CLI) with independent version
//! lines (`cosmokit` 1.x, `schemastery` 3.x, `cordis` 4.x) that never equal
//! the generation, so generation-number comparison flagged byte-identical
//! copies as red errors. Each profile copy is now compared against the same
//! package inside the CLI tree (the pnpm virtual store); a copy whose
//! package the tree does not provide at all gets an explicit
//! unknown-generation warning instead of a guessed verdict.
//!
//! The report is advisory: findings are logged and surfaced in the UI, never
//! used to block a launch.

use serde::Serialize;
use std::path::Path;

pub const HEALTH_EVENT: &str = "instance://health";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingLevel {
    Warn,
    Error,
}

#[derive(Clone, Debug, Serialize)]
pub struct DoctorFinding {
    pub level: FindingLevel,
    /// Stable machine-readable code (also used as the i18n key suffix).
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct DoctorReport {
    pub instance_id: String,
    pub profile: String,
    pub findings: Vec<DoctorFinding>,
}

/// Reads a package's version from `<dir>/package.json`.
fn package_version(pkg_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(pkg_dir.join("package.json")).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&raw).ok()?;
    doc.get("version")?.as_str().map(|s| s.to_string())
}

/// Version of the DSH CLI package inside an installed version tree. Source
/// checkouts (GitHub-only tags) keep it at the workspace path.
fn cli_core_version(version_dir: &Path) -> Option<String> {
    package_version(
        &version_dir
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh"),
    )
    .or_else(|| package_version(&version_dir.join("apps").join("cli")))
}

/// Versions of every `@deepseek-ai/*` package the CLI tree provides, as
/// (package id, version) pairs (issue #32). Scanned from the pnpm virtual
/// store: `<version_dir>/node_modules/.pnpm/` holds one directory per
/// package version named `@scope+name@version[_peer-suffix]`, so scope
/// packages appear as `@deepseek-ai+cosmokit@1.8.3`. This is the only place
/// the full dependency set is visible — top-level `node_modules/@deepseek-ai` only
/// carries direct dependencies (`dsh`), while transitive core packages
/// (cosmokit, schemastery, cordis, …) live solely in `.pnpm`.
fn cli_tree_versions(version_dir: &Path) -> Vec<(String, String)> {
    let store = version_dir.join("node_modules").join(".pnpm");
    let Ok(entries) = std::fs::read_dir(&store) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let dir_name = e.file_name().to_string_lossy().to_string();
            // `<pkg>@<version>` with the scope flattened to `@scope+name`;
            // the package name itself never contains `@`, so the first `@`
            // starts the version. Peer-dependency resolution suffixes follow
            // after `_` (or parens on some pnpm majors) — neither character
            // can occur inside a semver version, so truncating is safe.
            let rest = dir_name.strip_prefix("@deepseek-ai+")?;
            let at = rest.find('@')?;
            let (name, version) = rest.split_at(at);
            let version = &version[1..];
            let version = version.split(['_', '(']).next().unwrap_or(version);
            if name.is_empty() || version.is_empty() {
                return None;
            }
            Some((format!("@deepseek-ai/{name}"), version.to_string()))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Every `@deepseek-ai/*` package that has a copy inside the profile's
/// node_modules, as (package id, version) pairs. Normally empty: core comes
/// from the CLI tree, and the launcher never adds core packages to a profile.
fn profile_core_copies(profile_dir: &Path) -> Vec<(String, Option<String>)> {
    let scope = profile_dir.join("node_modules").join("@deepseek-ai");
    let Ok(entries) = std::fs::read_dir(&scope) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Option<String>)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            (format!("@deepseek-ai/{name}"), package_version(&e.path()))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Runs both checks. `selected_version` is the version string the instance is
/// configured to run (as recorded by the launcher).
pub fn inspect(
    instance_id: &str,
    profile: &str,
    version_dir: &Path,
    selected_version: &str,
    profile_dir: &Path,
) -> DoctorReport {
    let mut findings = Vec::new();
    let cli_core = cli_core_version(version_dir);

    // 1. The CLI tree's core version must match what the user selected.
    match &cli_core {
        Some(actual) if actual != selected_version => findings.push(DoctorFinding {
            level: FindingLevel::Warn,
            code: "core-version-mismatch".to_string(),
            message: format!(
                "实例选择的 DSH 版本为 {selected_version}，但 CLI 依赖树中的 @deepseek-ai/dsh 实际为 {actual}"
            ),
        }),
        None => findings.push(DoctorFinding {
            level: FindingLevel::Warn,
            code: "core-missing".to_string(),
            message: format!(
                "未能在版本目录中读取 @deepseek-ai/dsh 的版本信息：{}",
                version_dir.display()
            ),
        }),
        _ => {}
    }

    // 2. A profile must not carry core copies. Each copy is compared against
    //    the same package inside the CLI tree (issue #32): equal → a plain
    //    "should not be here" warning; different → the two-generations state
    //    and an error; not provided by the tree at all → the generation
    //    cannot be judged, so it stays a warning.
    let tree_versions = cli_tree_versions(version_dir);
    for (pkg, version) in profile_core_copies(profile_dir) {
        let tree = tree_versions
            .iter()
            .find(|(name, _)| name == &pkg)
            .map(|(_, v)| v.as_str());
        let shown = version.clone().unwrap_or_else(|| "未知版本".to_string());
        match (&version, tree) {
            // Copy equals the tree's own package: no mixed-generation risk.
            (Some(v), Some(tree_v)) if v == tree_v => {
                findings.push(DoctorFinding {
                    level: FindingLevel::Warn,
                    code: "profile-core-copy".to_string(),
                    message: format!(
                        "Profile「{profile}」的 node_modules 中存在核心包 {pkg}@{shown}；\
                         核心包应由 CLI 依赖树提供，profile 中不应出现"
                    ),
                });
            }
            // Copy differs from the tree's own package: two generations.
            (Some(_), Some(tree_v)) => {
                findings.push(DoctorFinding {
                    level: FindingLevel::Error,
                    code: "profile-core-mixed".to_string(),
                    message: format!(
                        "Profile「{profile}」的 node_modules 中存在核心包 {pkg}@{shown}，与 CLI 树中的 {tree_v} 不同代；\
                         该 profile 的工具调用可能全部失败，请卸载该包后重装插件；\
                         若该副本由插件的 dependencies 声明引入，需插件方将 {pkg} 移入 peerDependencies 后发版"
                    ),
                });
            }
            // The tree does not provide this package: generation unknown.
            (_, None) => {
                findings.push(DoctorFinding {
                    level: FindingLevel::Warn,
                    code: "profile-core-orphan".to_string(),
                    message: format!(
                        "Profile「{profile}」的 node_modules 中存在核心包 {pkg}@{shown}，\
                         但 CLI 依赖树未提供该包，无法判断代际；\
                         核心包应由 CLI 依赖树提供，profile 中不应出现"
                    ),
                });
            }
            // Copy with an unreadable version but the tree has the package.
            (None, Some(_)) => {
                findings.push(DoctorFinding {
                    level: FindingLevel::Warn,
                    code: "profile-core-copy".to_string(),
                    message: format!(
                        "Profile「{profile}」的 node_modules 中存在核心包 {pkg}（版本未知）；\
                         核心包应由 CLI 依赖树提供，profile 中不应出现"
                    ),
                });
            }
        }
    }

    DoctorReport {
        instance_id: instance_id.to_string(),
        profile: profile.to_string(),
        findings,
    }
}

/// Logs a report through the runtime log (errors as error, rest as warn).
pub fn log_report(report: &DoctorReport) {
    for f in &report.findings {
        match f.level {
            FindingLevel::Error => {
                crate::log_error!("[依赖自检] {} ({})", f.message, f.code)
            }
            FindingLevel::Warn => {
                crate::log_warn!("[依赖自检] {} ({})", f.message, f.code)
            }
        }
    }
    if report.findings.is_empty() {
        crate::log_debug!(
            "[依赖自检] 实例 {} / profile {} 未发现异常",
            report.instance_id,
            report.profile
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: std::path::PathBuf,
        version_dir: std::path::PathBuf,
        profile_dir: std::path::PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("dsh-doctor-{}", uuid::Uuid::new_v4()));
            let version_dir = root.join("versions").join("0.1.1-rc.2");
            let profile_dir = root.join("home").join("profiles").join("web");
            std::fs::create_dir_all(&version_dir).unwrap();
            std::fs::create_dir_all(&profile_dir).unwrap();
            Self {
                root,
                version_dir,
                profile_dir,
            }
        }

        fn write_pkg(&self, dir: &Path, name: &str, version: &str) {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(
                dir.join("package.json"),
                format!(r#"{{"name":"{name}","version":"{version}"}}"#),
            )
            .unwrap();
        }

        fn cli_core(&self, version: &str) {
            let dir = self
                .version_dir
                .join("node_modules")
                .join("@deepseek-ai")
                .join("dsh");
            self.write_pkg(&dir, "@deepseek-ai/dsh", version);
        }

        /// A package inside the CLI tree's pnpm virtual store, as pnpm lays
        /// it out: `.pnpm/@deepseek-ai+<name>@<version>/`.
        fn tree_pkg(&self, short_name: &str, version: &str) {
            let dir = self
                .version_dir
                .join("node_modules")
                .join(".pnpm")
                .join(format!("@deepseek-ai+{short_name}@{version}"));
            self.write_pkg(&dir, &format!("@deepseek-ai/{short_name}"), version);
        }

        fn profile_core(&self, short_name: &str, version: &str) {
            let dir = self
                .profile_dir
                .join("node_modules")
                .join("@deepseek-ai")
                .join(short_name);
            self.write_pkg(&dir, &format!("@deepseek-ai/{short_name}"), version);
        }

        fn run(&self, selected: &str) -> DoctorReport {
            inspect("i-1", "web", &self.version_dir, selected, &self.profile_dir)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    #[test]
    fn healthy_tree_reports_nothing() {
        let fx = Fixture::new();
        fx.cli_core("0.1.1-rc.2");
        let report = fx.run("0.1.1-rc.2");
        assert!(
            report.findings.is_empty(),
            "unexpected findings: {:?}",
            report.findings
        );
    }

    #[test]
    fn selected_version_mismatch_is_reported() {
        let fx = Fixture::new();
        fx.cli_core("0.1.0-rc.8");
        let report = fx.run("0.1.1-rc.2");
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].code, "core-version-mismatch");
        assert_eq!(report.findings[0].level, FindingLevel::Warn);
    }

    #[test]
    fn missing_cli_core_is_reported() {
        let fx = Fixture::new();
        let report = fx.run("0.1.1-rc.2");
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].code, "core-missing");
    }

    #[test]
    fn same_generation_profile_copy_is_a_warning() {
        // The tree provides dsh-tools@0.1.1-rc.2 and the profile carries the
        // same version: no mixed-generation risk, only a "should not be here"
        // warning. (Since issue #32 the comparison is per package against
        // the CLI tree, so the tree must actually provide the package.)
        let fx = Fixture::new();
        fx.cli_core("0.1.1-rc.2");
        fx.tree_pkg("dsh-tools", "0.1.1-rc.2");
        fx.profile_core("dsh-tools", "0.1.1-rc.2");
        let report = fx.run("0.1.1-rc.2");
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].code, "profile-core-copy");
        assert_eq!(report.findings[0].level, FindingLevel::Warn);
        assert!(report.findings[0]
            .message
            .contains("@deepseek-ai/dsh-tools"));
    }

    #[test]
    fn mixed_generation_profile_copy_is_an_error() {
        // The #4640 signature: the tree provides dsh-tools@0.1.1-rc.2, a
        // stale core copy in the profile is on 0.1.0-rc.8.
        let fx = Fixture::new();
        fx.cli_core("0.1.1-rc.2");
        fx.tree_pkg("dsh-tools", "0.1.1-rc.2");
        fx.profile_core("dsh-tools", "0.1.0-rc.8");
        let report = fx.run("0.1.1-rc.2");
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].code, "profile-core-mixed");
        assert_eq!(report.findings[0].level, FindingLevel::Error);
    }

    #[test]
    fn independent_version_line_copy_is_not_mixed_when_identical() {
        // Issue #32: cosmokit / schemastery follow their own version lines.
        // A profile copy identical to the tree's own package must be a plain
        // warning, never the red "different generations" error.
        let fx = Fixture::new();
        fx.cli_core("0.1.2-rc.1");
        fx.tree_pkg("cosmokit", "1.8.3");
        fx.tree_pkg("schemastery", "3.18.2");
        fx.profile_core("cosmokit", "1.8.3");
        fx.profile_core("schemastery", "3.18.2");
        let report = fx.run("0.1.2-rc.1");
        assert_eq!(report.findings.len(), 2, "{:?}", report.findings);
        assert!(report
            .findings
            .iter()
            .all(|f| f.code == "profile-core-copy" && f.level == FindingLevel::Warn));
    }

    #[test]
    fn independent_version_line_copy_differs_is_an_error() {
        // The tree provides cosmokit@1.8.3, the profile carries 1.7.0:
        // a genuine mixed-copy state, escalated to an error.
        let fx = Fixture::new();
        fx.cli_core("0.1.2-rc.1");
        fx.tree_pkg("cosmokit", "1.8.3");
        fx.profile_core("cosmokit", "1.7.0");
        let report = fx.run("0.1.2-rc.1");
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].code, "profile-core-mixed");
        assert_eq!(report.findings[0].level, FindingLevel::Error);
        // Error advice mentions the peerDependencies escape hatch.
        assert!(report.findings[0].message.contains("peerDependencies"));
    }

    #[test]
    fn package_absent_from_tree_is_an_orphan_warning() {
        // The tree provides no such package at all: the generation cannot be
        // judged, so the copy stays a warning (profile-core-orphan).
        let fx = Fixture::new();
        fx.cli_core("0.1.2-rc.1");
        fx.tree_pkg("cosmokit", "1.8.3");
        fx.profile_core("dsh-util-values", "0.1.2-rc.1");
        let report = fx.run("0.1.2-rc.1");
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].code, "profile-core-orphan");
        assert_eq!(report.findings[0].level, FindingLevel::Warn);
    }

    #[test]
    fn tree_scan_parses_pnpm_dir_names() {
        // Directory names in the virtual store: scope flattened with `+`,
        // peer-dependency suffixes joined with `_`, prereleases keep their
        // `-rc.N` tail.
        let fx = Fixture::new();
        fx.tree_pkg("cosmokit", "1.8.3");
        std::fs::create_dir_all(
            fx.version_dir
                .join("node_modules")
                .join(".pnpm")
                .join("@deepseek-ai+schemastery@3.18.2_peer@4.0.0"),
        )
        .unwrap();
        std::fs::create_dir_all(
            fx.version_dir
                .join("node_modules")
                .join(".pnpm")
                .join("@deepseek-ai+dsh-settings@0.1.2-rc.1"),
        )
        .unwrap();
        let versions = cli_tree_versions(&fx.version_dir);
        assert!(versions.contains(&("@deepseek-ai/cosmokit".to_string(), "1.8.3".to_string())));
        assert!(versions.contains(&("@deepseek-ai/schemastery".to_string(), "3.18.2".to_string())));
        assert!(versions.contains(&(
            "@deepseek-ai/dsh-settings".to_string(),
            "0.1.2-rc.1".to_string()
        )));
        // Nothing is invented for non-scope entries.
        assert_eq!(versions.len(), 3);
    }

    #[test]
    fn empty_scope_dir_is_not_a_finding() {
        let fx = Fixture::new();
        fx.cli_core("0.1.1-rc.2");
        std::fs::create_dir_all(fx.profile_dir.join("node_modules").join("@deepseek-ai")).unwrap();
        let report = fx.run("0.1.1-rc.2");
        assert!(report.findings.is_empty());
    }
}
