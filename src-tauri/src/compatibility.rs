//! Evidence-based compatibility inspection. Never mutates a profile.
use crate::doctor;
use crate::process::InstanceKind;
use serde::Serialize;
use serde_json::Value as Json;
use serde_yaml::Value as Yaml;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub code: String,
    pub category: String,
    pub severity: String,
    pub packages: Vec<String>,
    pub entries: Vec<String>,
    pub evidence: String,
    pub action: String,
    pub disable_entry: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Action {
    pub package: String,
    pub entry: String,
    pub reason: String,
    pub status: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub instance_id: String,
    pub profile: String,
    pub version: String,
    pub runtime: String,
    pub status: String,
    pub checked_at: String,
    pub findings: Vec<Finding>,
    pub initial_findings: Vec<Finding>,
    pub actions: Vec<Action>,
    pub unresolved: Vec<Finding>,
    pub started: bool,
    pub handoff_pending: bool,
}

impl Report {
    pub fn complete(&self) -> bool {
        self.status == "complete"
    }

    pub fn launchable(&self) -> bool {
        self.complete() && self.unresolved.is_empty()
    }

    pub fn refresh_unresolved(&mut self) {
        self.unresolved = self
            .findings
            .iter()
            .filter(|f| f.severity == "confirmed" || f.severity == "unknown")
            .cloned()
            .collect();
    }
}

#[derive(Clone)]
struct Entry {
    id: String,
    package: String,
    enabled: bool,
    user_insert: bool,
    certain: bool,
}

fn field<'a>(row: &'a Yaml, key: &str) -> Option<&'a Yaml> {
    row.as_mapping()?.get(Yaml::String(key.to_string()))
}

fn text<'a>(row: &'a Yaml, key: &str) -> Option<&'a str> {
    field(row, key)?.as_str()
}

fn read_json(path: &Path) -> Result<Json, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_patch(path: &Path, optional: bool) -> Result<Vec<Yaml>, String> {
    let raw = match std::fs::read_to_string(path) {
        Err(e) if optional && e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        result => result.map_err(|e| format!("{}: {e}", path.display()))?,
    };
    let doc: Yaml = serde_yaml::from_str(&raw).map_err(|e| format!("{}: {e}", path.display()))?;
    match doc {
        Yaml::Sequence(rows) => Ok(rows),
        _ => Err(format!("{}: expected a patch array", path.display())),
    }
}

fn installed_package(profile: &Path, version: &Path, name: &str, bundle: bool) -> Option<PathBuf> {
    if name.is_empty()
        || name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == ".." || part.contains('\\'))
    {
        return None;
    }
    let sub = Path::new("node_modules").join(name);
    let paths = if bundle {
        [version.join(&sub), profile.join(&sub)]
    } else {
        [profile.join(&sub), version.join(&sub)]
    };
    paths.into_iter().find(|p| p.join("package.json").exists())
}

fn finding(
    code: &str,
    severity: &str,
    packages: Vec<String>,
    entries: Vec<String>,
    evidence: String,
    action: &str,
    disable_entry: Option<String>,
) -> Finding {
    Finding {
        code: code.into(),
        category: "plugin".into(),
        severity: severity.into(),
        packages,
        entries,
        evidence,
        action: action.into(),
        disable_entry,
    }
}

fn apply_rows(
    entries: &mut Vec<Entry>,
    rows: &[Yaml],
    user: bool,
    source: &str,
    issues: &mut Vec<Finding>,
) {
    for row in rows {
        if let Some(insert) = field(row, "insert") {
            if field(row, "id").is_some() {
                issues.push(finding(
                    "group-insert",
                    "unknown",
                    vec![],
                    vec![],
                    source.into(),
                    "Inspect group-targeted inserts with the DSH loader",
                    None,
                ));
                continue;
            }
            let Some(children) = insert.as_sequence() else {
                issues.push(finding(
                    "invalid-insert",
                    "unknown",
                    vec![],
                    vec![],
                    source.into(),
                    "Correct the patch insert list",
                    None,
                ));
                continue;
            };
            for child in children {
                let (Some(id), Some(package)) = (text(child, "id"), text(child, "name")) else {
                    issues.push(finding(
                        "unknown-entry",
                        "unknown",
                        vec![],
                        vec![],
                        source.into(),
                        "Inspect the inserted entry",
                        None,
                    ));
                    continue;
                };
                let disabled = field(child, "disabled");
                entries.push(Entry {
                    id: id.into(),
                    package: package.into(),
                    enabled: disabled != Some(&Yaml::Bool(true)),
                    user_insert: user,
                    certain: disabled.is_none()
                        || disabled == Some(&Yaml::Bool(true))
                        || disabled == Some(&Yaml::Bool(false)),
                });
            }
        } else if let Some(id) = text(row, "id") {
            if field(row, "name").is_some() {
                issues.push(finding(
                    "guarded-override",
                    "unknown",
                    vec![],
                    vec![id.into()],
                    source.into(),
                    "Verify the override name against the loader",
                    None,
                ));
                continue;
            }
            let targets: Vec<_> = entries.iter_mut().filter(|e| e.id == id).collect();
            if targets.is_empty() {
                issues.push(finding(
                    "unmatched-override",
                    "warning",
                    vec![],
                    vec![id.into()],
                    source.into(),
                    "Review the patch target",
                    None,
                ));
            }
            for target in targets {
                if let Some(disabled) = field(row, "disabled") {
                    if let Some(value) = disabled.as_bool() {
                        target.enabled = !value;
                    } else {
                        target.certain = false;
                    }
                }
            }
        } else {
            issues.push(finding(
                "unknown-patch",
                "unknown",
                vec![],
                vec![],
                source.into(),
                "Inspect unsupported patch syntax",
                None,
            ));
        }
    }
}

fn version_matches(req: &str, actual: &str) -> Option<bool> {
    let version = semver::Version::parse(actual).ok()?;
    let requirement = semver::VersionReq::parse(req).ok()?;
    if !version.pre.is_empty() && !req.contains('-') {
        return None;
    }
    Some(requirement.matches(&version))
}

/// Snapshot the selected installed CLI and profile. A failed required input is
/// returned as an incomplete report, never as a clean scan.
pub fn inspect(
    instance_id: &str,
    profile: &str,
    version_dir: &Path,
    version: &str,
    profile_dir: &Path,
    runtime: &str,
) -> Report {
    let mut report = Report {
        instance_id: instance_id.into(),
        profile: profile.into(),
        version: version.into(),
        runtime: runtime.into(),
        status: "complete".into(),
        checked_at: chrono::Utc::now().to_rfc3339(),
        findings: vec![],
        initial_findings: vec![],
        actions: vec![],
        unresolved: vec![],
        started: false,
        handoff_pending: false,
    };
    let manifest = read_json(&profile_dir.join("package.json"));
    let cli = read_json(&version_dir.join("node_modules/@deepseek-ai/dsh/package.json"))
        .or_else(|_| read_json(&version_dir.join("apps/cli/package.json")));
    if manifest.is_err() || cli.is_err() {
        report.status = "failed".into();
        report.findings.push(finding(
            "required-input",
            "unknown",
            vec![],
            vec![],
            format!(
                "profile: {}; CLI: {}",
                manifest.as_ref().err().cloned().unwrap_or_default(),
                cli.as_ref().err().cloned().unwrap_or_default()
            ),
            "Repair the installed profile or DSH version",
            None,
        ));
        report.refresh_unresolved();
        return report;
    }
    let manifest = manifest.unwrap();
    let cli = cli.unwrap();
    if cli.get("version").and_then(Json::as_str) != Some(version) {
        report.findings.push(finding(
            "core-version-mismatch",
            "warning",
            vec!["@deepseek-ai/dsh".into()],
            vec![],
            format!("selected {version}, installed {}", cli["version"]),
            "Verify the selected DSH installation",
            None,
        ));
    }
    let bundles = match manifest
        .pointer("/dsh/profile/bundles")
        .and_then(Json::as_array)
    {
        Some(bundles) if bundles.iter().all(Json::is_string) => bundles,
        _ => {
            report.status = "failed".into();
            report.findings.push(finding(
                "invalid-profile",
                "unknown",
                vec![],
                vec![],
                "dsh.profile.bundles must be an array of package names".into(),
                "Repair package.json",
                None,
            ));
            report.refresh_unresolved();
            return report;
        }
    };
    let mut entries = vec![];
    for bundle in bundles {
        let name = bundle.as_str().unwrap();
        let Some(dir) = installed_package(profile_dir, version_dir, name, true) else {
            report.status = "partial".into();
            report.findings.push(finding(
                "missing-bundle",
                "unknown",
                vec![name.into()],
                vec![],
                "bundle package is not installed".into(),
                "Reinstall the bundle",
                None,
            ));
            continue;
        };
        let bundle_manifest = read_json(&dir.join("package.json"));
        let Ok(bundle_manifest) = bundle_manifest else {
            report.status = "partial".into();
            report.findings.push(finding(
                "unreadable-bundle",
                "unknown",
                vec![name.into()],
                vec![],
                dir.display().to_string(),
                "Repair the bundle installation",
                None,
            ));
            continue;
        };
        let Some(patch) = bundle_manifest
            .pointer("/dsh/bundle/patch")
            .and_then(Json::as_str)
        else {
            report.status = "partial".into();
            report.findings.push(finding(
                "missing-bundle-patch",
                "unknown",
                vec![name.into()],
                vec![],
                dir.display().to_string(),
                "Repair the bundle manifest",
                None,
            ));
            continue;
        };
        match read_patch(&dir.join(patch), false) {
            Ok(rows) => apply_rows(&mut entries, &rows, false, name, &mut report.findings),
            Err(e) => {
                report.status = "partial".into();
                report.findings.push(finding(
                    "unreadable-bundle-patch",
                    "unknown",
                    vec![name.into()],
                    vec![],
                    e,
                    "Repair the bundle patch",
                    None,
                ));
            }
        }
    }
    match read_patch(&profile_dir.join("cordis.patch.yml"), true) {
        Ok(rows) => apply_rows(
            &mut entries,
            &rows,
            true,
            "user patch",
            &mut report.findings,
        ),
        Err(e) => {
            report.status = "partial".into();
            report.findings.push(finding(
                "unreadable-user-patch",
                "unknown",
                vec![],
                vec![],
                e,
                "Repair the profile patch",
                None,
            ));
        }
    }
    if report.findings.iter().any(|f| f.severity == "unknown") {
        report.status = "partial".into();
    }

    let core_store = version_dir.join("node_modules/.pnpm");
    if std::fs::read_dir(&core_store).is_err() {
        report.findings.push(finding(
            "unreadable-core-tree",
            "unknown",
            vec![],
            vec![],
            core_store.display().to_string(),
            "Inspect the installed CLI tree",
            None,
        ));
    }
    let core_scope = profile_dir.join("node_modules/@deepseek-ai");
    if core_scope.exists() && std::fs::read_dir(&core_scope).is_err() {
        report.findings.push(finding(
            "unreadable-profile-core",
            "unknown",
            vec![],
            vec![],
            core_scope.display().to_string(),
            "Inspect the profile dependency tree",
            None,
        ));
    }
    let core = doctor::inspect(instance_id, profile, version_dir, version, profile_dir);
    for f in core.findings {
        report.findings.push(Finding {
            code: f.code,
            category: "core-tree".into(),
            severity: if f.level == doctor::FindingLevel::Error {
                "confirmed"
            } else {
                "warning"
            }
            .into(),
            packages: vec![],
            entries: vec![],
            evidence: f.message,
            action: "Repair the dependency tree manually".into(),
            disable_entry: None,
        });
    }

    let mut by_id: BTreeMap<&str, Vec<&Entry>> = BTreeMap::new();
    for entry in entries.iter().filter(|e| e.enabled) {
        by_id.entry(&entry.id).or_default().push(entry);
    }
    for (id, owners) in by_id {
        if owners.len() > 1 {
            report.findings.push(finding(
                "duplicate-entry",
                "confirmed",
                owners.iter().map(|e| e.package.clone()).collect(),
                vec![id.into()],
                format!("{} enabled entries use Cordis ID {id}", owners.len()),
                "Choose a plugin to disable manually",
                None,
            ));
        }
    }

    let kind = if bundles
        .iter()
        .any(|b| b.as_str() == Some("@deepseek-ai/dsh-web-app"))
    {
        InstanceKind::Web
    } else if bundles
        .iter()
        .any(|b| b.as_str() == Some("@deepseek-harness-tui/dsh-tui"))
    {
        InstanceKind::Tui
    } else {
        InstanceKind::Other
    };
    let surface = match kind {
        InstanceKind::Web => "web",
        InstanceKind::Tui => "tui",
        InstanceKind::Other => "other",
    };
    let active_packages: std::collections::BTreeSet<&str> = entries
        .iter()
        .filter(|e| e.enabled)
        .map(|e| e.package.as_str())
        .collect();
    for entry in entries.iter().filter(|e| e.enabled) {
        let owners = entries
            .iter()
            .filter(|e| e.enabled && e.id == entry.id)
            .count();
        let package_owners = entries
            .iter()
            .filter(|e| e.enabled && e.package == entry.package)
            .count();
        let safe = entry.user_insert
            && entry.certain
            && owners == 1
            && package_owners == 1
            && !entry.package.starts_with("@deepseek-ai/")
            && !bundles.iter().any(|b| b.as_str() == Some(&entry.package));
        let disable = safe.then(|| entry.id.clone());
        if !entry.certain {
            report.findings.push(finding(
                "dynamic-disabled",
                "unknown",
                vec![entry.package.clone()],
                vec![entry.id.clone()],
                "disabled is not a static boolean".into(),
                "Check the runtime expression manually",
                None,
            ));
        }
        let Some(dir) =
            installed_package(profile_dir, version_dir, &entry.package, !entry.user_insert)
        else {
            report.findings.push(finding(
                "missing-package",
                "unknown",
                vec![entry.package.clone()],
                vec![entry.id.clone()],
                "entry package is not resolvable from this installation".into(),
                "Inspect the entry's module resolution",
                None,
            ));
            continue;
        };
        let Ok(pkg) = read_json(&dir.join("package.json")) else {
            report.findings.push(finding(
                "unreadable-package",
                "unknown",
                vec![entry.package.clone()],
                vec![entry.id.clone()],
                dir.display().to_string(),
                "Repair package metadata",
                None,
            ));
            continue;
        };
        if pkg.get("name").and_then(Json::as_str) != Some(&entry.package) {
            report.findings.push(finding(
                "package-identity",
                "unknown",
                vec![entry.package.clone()],
                vec![entry.id.clone()],
                dir.display().to_string(),
                "Verify the installed package identity",
                None,
            ));
            continue;
        }
        if let Some(peers) = pkg.get("peerDependencies").and_then(Json::as_object) {
            for (peer, constraint) in peers {
                if peer == "@deepseek-ai/dsh" || !active_packages.contains(peer.as_str()) {
                    continue;
                }
                let Some(peer_dir) = installed_package(profile_dir, version_dir, peer, false)
                else {
                    report.findings.push(finding(
                        "unresolved-peer",
                        "unknown",
                        vec![entry.package.clone(), peer.clone()],
                        vec![entry.id.clone()],
                        "An enabled peer package is not resolvable".into(),
                        "Inspect the installed dependency tree",
                        None,
                    ));
                    continue;
                };
                let Ok(peer_pkg) = read_json(&peer_dir.join("package.json")) else {
                    report.findings.push(finding(
                        "unreadable-peer",
                        "unknown",
                        vec![entry.package.clone(), peer.clone()],
                        vec![entry.id.clone()],
                        peer_dir.display().to_string(),
                        "Inspect peer metadata",
                        None,
                    ));
                    continue;
                };
                match (
                    constraint.as_str(),
                    peer_pkg.get("version").and_then(Json::as_str),
                ) {
                    (Some(req), Some(actual)) => match version_matches(req, actual) {
                        Some(false) => report.findings.push(finding(
                            "plugin-peer-version",
                            "confirmed",
                            vec![entry.package.clone(), peer.clone()],
                            vec![entry.id.clone()],
                            format!(
                                "{} requires {peer}@{req}, installed {actual}",
                                entry.package
                            ),
                            "Choose a compatible plugin version manually",
                            None,
                        )),
                        None => report.findings.push(finding(
                            "unknown-peer-range",
                            "unknown",
                            vec![entry.package.clone(), peer.clone()],
                            vec![entry.id.clone()],
                            format!("{req}; installed {actual}"),
                            "Verify the peer range manually",
                            None,
                        )),
                        _ => {}
                    },
                    _ => report.findings.push(finding(
                        "unknown-peer-metadata",
                        "unknown",
                        vec![entry.package.clone(), peer.clone()],
                        vec![entry.id.clone()],
                        peer_dir.display().to_string(),
                        "Repair the peer metadata",
                        None,
                    )),
                }
            }
        }
        let declared = pkg
            .get("peerDependencies")
            .and_then(|p| p.get("@deepseek-ai/dsh"))
            .and_then(Json::as_str)
            .or_else(|| pkg.pointer("/engines/dsh").and_then(Json::as_str));
        if let Some(req) = declared {
            match version_matches(req, version) {
                Some(false) => report.findings.push(finding(
                    "dsh-version",
                    "confirmed",
                    vec![entry.package.clone()],
                    vec![entry.id.clone()],
                    format!(
                        "{} declares {req}; selected DSH is {version}",
                        dir.display()
                    ),
                    "Disable this entry or install a compatible version",
                    disable.clone(),
                )),
                None => report.findings.push(finding(
                    "unknown-version-range",
                    "unknown",
                    vec![entry.package.clone()],
                    vec![entry.id.clone()],
                    req.into(),
                    "Check the declared range manually",
                    None,
                )),
                _ => {}
            }
        } else {
            report.findings.push(finding(
                "missing-compatibility-metadata",
                "warning",
                vec![entry.package.clone()],
                vec![entry.id.clone()],
                dir.display().to_string(),
                "Check plugin support with its maintainer",
                None,
            ));
        }
        for (key, actual) in [("surface", surface), ("platform", runtime)] {
            if let Some(supported) = pkg.pointer(&format!("/dsh/runtime/{key}")) {
                if let Some(values) = supported
                    .as_array()
                    .filter(|v| v.iter().all(Json::is_string))
                {
                    if !values.iter().any(|v| v.as_str() == Some(actual)) {
                        report.findings.push(finding(
                            "runtime-incompatible",
                            "confirmed",
                            vec![entry.package.clone()],
                            vec![entry.id.clone()],
                            format!(
                                "{} declares {key}: {supported}; selected {actual}",
                                dir.display()
                            ),
                            "Disable this entry or choose a supported runtime",
                            disable.clone(),
                        ));
                    }
                } else {
                    report.findings.push(finding(
                        "unknown-runtime-metadata",
                        "unknown",
                        vec![entry.package.clone()],
                        vec![entry.id.clone()],
                        supported.to_string(),
                        "Correct the runtime metadata",
                        None,
                    ));
                }
            }
        }
    }
    // Dynamic entries and unsupported patch operations cannot certify the scan.
    if report.findings.iter().any(|f| f.severity == "unknown") {
        report.status = "partial".into();
    }
    report.refresh_unresolved();
    report
}

pub fn disable_candidates(report: &Report) -> Vec<Action> {
    let mut actions = BTreeMap::new();
    for f in report.findings.iter().filter(|f| f.severity == "confirmed") {
        if let (Some(entry), Some(package)) = (&f.disable_entry, f.packages.first()) {
            actions.entry(entry.clone()).or_insert_with(|| Action {
                package: package.clone(),
                entry: entry.clone(),
                reason: f.code.clone(),
                status: "proposed".into(),
            });
        }
    }
    actions.into_values().collect()
}

pub fn disable_patch(path: &Path, actions: &[Action]) -> Result<(), String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut next = raw.clone();
    for action in actions {
        next = crate::plugins::set_disabled_row(&next, &action.entry, false);
    }
    if next == raw {
        return Ok(());
    }
    let backup = path.with_extension("issue66.bak");
    if backup.exists() {
        return Err(format!(
            "Recovery backup already exists: {}",
            backup.display()
        ));
    }
    std::fs::copy(path, &backup).map_err(|e| format!("Backup {}: {e}", backup.display()))?;
    if let Err(e) = std::fs::write(path, next) {
        std::fs::copy(&backup, path).map_err(|restore| {
            format!(
                "Write failed: {e}; restore failed: {restore}; recover from {}",
                backup.display()
            )
        })?;
        return Err(format!(
            "Write failed: {e}; backup retained at {}",
            backup.display()
        ));
    }
    std::fs::remove_file(&backup)
        .map_err(|e| format!("Remove backup {}: {e}", backup.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: PathBuf,
        version: PathBuf,
        profile: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("dsh-compat-{}", uuid::Uuid::new_v4()));
            let version = root.join("version");
            let profile = root.join("home/profiles/web");
            std::fs::create_dir_all(version.join("node_modules/@deepseek-ai/dsh")).unwrap();
            std::fs::create_dir_all(version.join("node_modules/.pnpm")).unwrap();
            std::fs::create_dir_all(&profile).unwrap();
            std::fs::write(
                version.join("node_modules/@deepseek-ai/dsh/package.json"),
                r#"{"name":"@deepseek-ai/dsh","version":"1.2.0-rc.1"}"#,
            )
            .unwrap();
            std::fs::write(
                profile.join("package.json"),
                r#"{"dsh":{"profile":{"bundles":[]}}}"#,
            )
            .unwrap();
            std::fs::write(profile.join("cordis.patch.yml"), "[]\n").unwrap();
            Self {
                root,
                version,
                profile,
            }
        }
        fn package(&self, name: &str, metadata: &str) {
            let dir = self.profile.join("node_modules").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let mut doc: Json = serde_json::from_str(metadata).unwrap();
            doc["name"] = Json::String(name.into());
            std::fs::write(dir.join("package.json"), doc.to_string()).unwrap();
        }
        fn patch(&self, patch: &str) {
            std::fs::write(self.profile.join("cordis.patch.yml"), patch).unwrap();
        }
        fn inspect(&self) -> Report {
            inspect(
                "instance",
                "web",
                &self.version,
                "1.2.0-rc.1",
                &self.profile,
                "windows",
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    #[test]
    fn clean_and_read_only() {
        let fx = Fixture::new();
        let before = std::fs::read(fx.profile.join("cordis.patch.yml")).unwrap();
        let report = fx.inspect();
        assert!(report.launchable());
        assert!(report.findings.is_empty());
        assert_eq!(
            before,
            std::fs::read(fx.profile.join("cordis.patch.yml")).unwrap()
        );
    }

    #[test]
    fn incompatible_insert_is_exactly_disabled_and_can_be_reenabled() {
        let fx = Fixture::new();
        fx.package(
            "third-party",
            r#"{"version":"1.0.0","peerDependencies":{"@deepseek-ai/dsh":">=2.0.0-rc.1"}}"#,
        );
        fx.patch("- insert:\n    - id: third\n      name: third-party\n      config:\n        nested: true\n    - id: other\n      name: unrelated\n");
        let report = fx.inspect();
        let actions = disable_candidates(&report);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].entry, "third");
        disable_patch(&fx.profile.join("cordis.patch.yml"), &actions).unwrap();
        disable_patch(&fx.profile.join("cordis.patch.yml"), &actions).unwrap();
        let raw = std::fs::read_to_string(fx.profile.join("cordis.patch.yml")).unwrap();
        assert!(raw.contains("nested: true"));
        assert!(raw.contains("- id: other"));
        assert!(raw.contains("- id: third\n      disabled: true"));
        assert!(!fx
            .inspect()
            .findings
            .iter()
            .any(|f| f.code == "dsh-version"));
        let enabled = crate::plugins::set_disabled_row(&raw, "third", true);
        assert!(!enabled.contains("disabled: true"));
    }

    #[test]
    fn duplicate_id_has_no_automatic_victim() {
        let fx = Fixture::new();
        for name in ["first", "second"] {
            fx.package(name, r#"{"version":"1.0.0"}"#);
        }
        fx.patch("- insert:\n    - id: shared\n      name: first\n    - id: shared\n      name: second\n");
        let report = fx.inspect();
        assert!(report.findings.iter().any(|f| f.code == "duplicate-entry"));
        assert!(disable_candidates(&report).is_empty());
        assert!(!report.launchable());
    }

    #[test]
    fn runtime_mismatch_and_missing_metadata() {
        let fx = Fixture::new();
        fx.package(
            "limited",
            r#"{"version":"1.0.0","dsh":{"runtime":{"platform":["wsl"],"surface":["tui"]}}}"#,
        );
        fx.patch("- insert:\n    - id: limited\n      name: limited\n");
        let report = fx.inspect();
        assert_eq!(
            report
                .findings
                .iter()
                .filter(|f| f.code == "runtime-incompatible")
                .count(),
            2
        );
        assert!(report
            .findings
            .iter()
            .any(|f| f.code == "missing-compatibility-metadata"));
    }

    #[test]
    fn ambiguous_and_unassessable_inputs_fail_closed() {
        let fx = Fixture::new();
        fx.package(
            "p",
            r#"{"version":"1.0.0","peerDependencies":{"@deepseek-ai/dsh":">=1.0.0"}}"#,
        );
        fx.patch("- insert:\n    - id: p\n      name: p\n      disabled: !!js true\n");
        let report = fx.inspect();
        assert_eq!(report.status, "partial");
        assert!(disable_candidates(&report).is_empty());
        assert!(report
            .findings
            .iter()
            .any(|f| f.code == "unknown-version-range"));
        fx.patch("- insert: [\n");
        assert!(fx
            .inspect()
            .findings
            .iter()
            .any(|f| f.code == "unreadable-user-patch"));
        std::fs::write(fx.profile.join("package.json"), "{").unwrap();
        assert_eq!(fx.inspect().status, "failed");
    }

    #[test]
    fn prerelease_requires_explicit_range_and_disabled_entries_are_ignored() {
        let fx = Fixture::new();
        fx.package(
            "p",
            r#"{"version":"1.0.0","peerDependencies":{"@deepseek-ai/dsh":">=1.0.0"}}"#,
        );
        fx.patch("- insert:\n    - id: p\n      name: p\n");
        assert!(fx
            .inspect()
            .findings
            .iter()
            .any(|f| f.code == "unknown-version-range"));
        fx.patch("- insert:\n    - id: p\n      name: p\n      disabled: true\n");
        assert!(fx.inspect().launchable());
    }

    #[test]
    fn bundle_layers_and_user_override_follow_order() {
        let fx = Fixture::new();
        let bundle = fx.version.join("node_modules/@deepseek-ai/dsh-base");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(
            bundle.join("package.json"),
            r#"{"name":"@deepseek-ai/dsh-base","dsh":{"bundle":{"patch":"./cordis.patch.yml"}}}"#,
        )
        .unwrap();
        std::fs::write(
            bundle.join("cordis.patch.yml"),
            "- insert:\n    - id: bundled\n      name: @deepseek-ai/dsh-base\n",
        )
        .unwrap();
        std::fs::write(
            fx.profile.join("package.json"),
            r#"{"dsh":{"profile":{"bundles":["@deepseek-ai/dsh-base"]}}}"#,
        )
        .unwrap();
        fx.patch("- id: bundled\n  disabled: true\n");
        let report = fx.inspect();
        assert!(!report.findings.iter().any(|f| f.code == "missing-package"));
        assert!(disable_candidates(&report).is_empty());
    }

    #[test]
    fn ambiguous_package_mapping_never_disables() {
        let fx = Fixture::new();
        fx.package(
            "p",
            r#"{"version":"1.0.0","dsh":{"runtime":{"platform":["wsl"]}}}"#,
        );
        fx.patch("- insert:\n    - id: one\n      name: p\n    - id: two\n      name: p\n");
        let report = fx.inspect();
        assert!(report
            .findings
            .iter()
            .any(|f| f.code == "runtime-incompatible"));
        assert!(disable_candidates(&report).is_empty());
    }

    #[test]
    fn unreadable_core_tree_and_mixed_copy_are_not_auto_fixed() {
        let fx = Fixture::new();
        std::fs::remove_dir_all(fx.version.join("node_modules/.pnpm")).unwrap();
        let core = fx.profile.join("node_modules/@deepseek-ai/dsh-tools");
        std::fs::create_dir_all(&core).unwrap();
        std::fs::write(
            core.join("package.json"),
            r#"{"name":"@deepseek-ai/dsh-tools","version":"1.0.0"}"#,
        )
        .unwrap();
        let report = fx.inspect();
        assert_eq!(report.status, "partial");
        assert!(report
            .findings
            .iter()
            .any(|f| f.code == "unreadable-core-tree"));
        assert!(disable_candidates(&report).is_empty());
        let store = fx
            .version
            .join("node_modules/.pnpm/@deepseek-ai+dsh-tools@2.0.0");
        std::fs::create_dir_all(&store).unwrap();
        let report = fx.inspect();
        assert!(report
            .findings
            .iter()
            .any(|f| f.code == "profile-core-mixed"));
        assert!(disable_candidates(&report).is_empty());
    }

    #[test]
    fn failed_backup_prevents_mutation() {
        let fx = Fixture::new();
        let path = fx.profile.join("cordis.patch.yml");
        let backup = path.with_extension("issue66.bak");
        std::fs::write(&backup, "recovery").unwrap();
        fx.patch("- insert:\n    - id: p\n      name: p\n");
        let before = std::fs::read(&path).unwrap();
        let action = Action {
            package: "p".into(),
            entry: "p".into(),
            reason: "dsh-version".into(),
            status: "proposed".into(),
        };
        assert!(disable_patch(&path, &[action]).is_err());
        assert_eq!(std::fs::read(path).unwrap(), before);
        assert_eq!(std::fs::read(backup).unwrap(), b"recovery");
    }

    #[test]
    fn incompatible_peer_between_plugins_is_report_only() {
        let fx = Fixture::new();
        fx.package(
            "one",
            r#"{"version":"1.0.0","peerDependencies":{"two":">=2.0.0"}}"#,
        );
        fx.package("two", r#"{"version":"1.0.0"}"#);
        fx.patch("- insert:\n    - id: one\n      name: one\n    - id: two\n      name: two\n");
        let report = fx.inspect();
        assert!(report
            .findings
            .iter()
            .any(|f| f.code == "plugin-peer-version" && f.severity == "confirmed"));
        assert!(disable_candidates(&report).is_empty());
    }
}
