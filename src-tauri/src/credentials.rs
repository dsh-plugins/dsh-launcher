//! Credential store access for `$DSH_HOME/.credentials.yaml`.
//!
//! DSH resolves a credential reference (`apiKeyEnv`) through four layers, in
//! this order, and the launcher mirrors the same order so the models page can
//! tell the user whether a key is configured and where it came from:
//!
//! 1. the launch environment — read-only, outranks everything
//! 2. `<DSH_HOME>/.credentials.yaml` (`refs`) — the only writable layer
//! 3. the profile `.env`
//! 4. the DSH_HOME `.env`
//!
//! Secrets cross in one direction only: nothing here ever returns a stored
//! value, only a [`CredentialInfo`] descriptor (`configured` / `source` /
//! `writable`), which is exactly what DSH's `credentials/describe` hands to
//! its own models page.
//!
//! Writing never re-serializes the document. The store's schema is strict
//! (only `version` / `refs` / `records` at the top level, `version` must be
//! `1`) and a malformed file stops DSH from booting at all, so writes splice
//! one line out of / into the raw text and keep every comment, the `records`
//! block and the key order byte-for-byte.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Credential store filename inside a DSH_HOME.
pub const CREDENTIALS_FILENAME: &str = ".credentials.yaml";

/// Top-level keys DSH accepts; anything else makes the store unreadable.
const KNOWN_TOP_LEVEL_KEYS: [&str; 3] = ["version", "refs", "records"];

/// Mirrors `@deepseek-ai/dsh-credentials`' `CredentialInfo`: a descriptor,
/// never the secret.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CredentialInfo {
    pub configured: bool,
    /// `"env"` | `"file"` | `"project-env"` | `"user-env"`; `None` when unset.
    pub source: Option<String>,
    /// Only the `file` layer can be written through the launcher.
    pub writable: bool,
    /// Launcher-only: the reference is supplied by the instance's env
    /// overrides, which outrank the store and are read-only (issue #89).
    pub overridden_by_instance: bool,
}

impl CredentialInfo {
    fn unconfigured() -> Self {
        Self {
            configured: false,
            source: None,
            writable: true,
            overridden_by_instance: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Grammar (mirrors DSH)
// ---------------------------------------------------------------------------

/// A credential reference is a POSIX environment-variable name.
pub fn is_valid_reference(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    if !(bytes[0].is_ascii_alphabetic() || bytes[0] == b'_') {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

/// A secret is printable ASCII without whitespace (`/^[\x21-\x7E]+$/` in DSH),
/// which is why a single-quoted YAML scalar is always safe for it.
pub fn is_valid_secret(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

fn validate_reference(name: &str) -> Result<(), String> {
    if is_valid_reference(name) {
        Ok(())
    } else {
        Err(format!("凭据引用名 {name} 不是合法的环境变量名"))
    }
}

fn validate_secret(value: &str) -> Result<(), String> {
    if is_valid_secret(value) {
        Ok(())
    } else {
        Err("API 密钥含有空格或不可打印字符，请检查。".to_string())
    }
}

/// Refuses to store a secret the launch environment already supplies: writing
/// would succeed but never take effect, which is the worst kind of feedback.
pub fn ensure_writable(info: &CredentialInfo) -> Result<(), String> {
    if info.overridden_by_instance {
        Err("该凭据由启动环境提供（只读），无法在此写入。".to_string())
    } else if !info.writable {
        Err("该凭据所在的层不可写入。".to_string())
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

pub fn store_path(home: &Path) -> PathBuf {
    home.join(CREDENTIALS_FILENAME)
}

fn read_optional(path: &Path) -> Result<Option<String>, String> {
    if !path.exists() {
        return Ok(None);
    }
    std::fs::read_to_string(path)
        .map(Some)
        .map_err(|e| format!("读取 {CREDENTIALS_FILENAME} 失败: {e}"))
}

/// Rejects any store the launcher cannot safely splice: DSH refuses documents
/// without `version: 1` or with unknown top-level keys outright, so writing
/// one would break the instance.
fn validate_store(raw: &str, path: &Path) -> Result<(), String> {
    let doc: serde_yaml::Value = serde_yaml::from_str(raw)
        .map_err(|e| format!("{} 不是合法的 YAML: {e}", path.display()))?;
    let Some(map) = doc.as_mapping() else {
        return Err(format!("{} 的顶层不是映射", path.display()));
    };
    let Some(version) = map.get("version") else {
        return Err(format!(
            "{} 缺少 version: 1，启动器无法安全写入",
            path.display()
        ));
    };
    if version.as_u64() != Some(1) {
        return Err(format!(
            "{} 声明的 version 不是 1，启动器只支持 version 1",
            path.display()
        ));
    }
    for key in map.keys() {
        let name = key.as_str().unwrap_or_default().to_string();
        if !KNOWN_TOP_LEVEL_KEYS.contains(&name.as_str()) {
            return Err(format!(
                "{} 含有未知顶层字段 {name}，启动器拒绝写入",
                path.display()
            ));
        }
    }
    Ok(())
}

fn refs_of(raw: &str) -> serde_yaml::Mapping {
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(raw) else {
        return serde_yaml::Mapping::new();
    };
    doc.get("refs")
        .and_then(|v| v.as_mapping())
        .cloned()
        .unwrap_or_default()
}

/// Stored value for one reference; an empty string counts as unset.
pub(crate) fn stored_value(raw: &str, reference: &str) -> Option<String> {
    let value = refs_of(raw).get(reference)?.as_str()?.to_string();
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Best-effort `KEY=VALUE` probe for the `.env` layers. Only presence matters,
/// so a malformed line is skipped rather than fatal.
fn env_file_has(path: &Path, reference: &str) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    raw.lines().any(|line| {
        let line = line.trim();
        if line.starts_with('#') {
            return false;
        }
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        key.trim() == reference && !value.trim().is_empty()
    })
}

/// Four-layer resolution, mirroring `@deepseek-ai/dsh-credentials-local`.
pub fn describe(
    reference: &str,
    env_overrides: &BTreeMap<String, String>,
    home: &Path,
    profile: Option<&str>,
) -> Result<CredentialInfo, String> {
    if env_overrides
        .get(reference)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        return Ok(CredentialInfo {
            configured: true,
            source: Some("env".to_string()),
            writable: false,
            overridden_by_instance: true,
        });
    }

    let path = store_path(home);
    if let Some(raw) = read_optional(&path)? {
        if !raw.trim().is_empty() {
            validate_store(&raw, &path)?;
            if stored_value(&raw, reference).is_some() {
                return Ok(CredentialInfo {
                    configured: true,
                    source: Some("file".to_string()),
                    writable: true,
                    overridden_by_instance: false,
                });
            }
        }
    }

    if let Some(profile) = profile {
        let env = home.join("profiles").join(profile).join(".env");
        if env_file_has(&env, reference) {
            return Ok(CredentialInfo {
                configured: true,
                source: Some("project-env".to_string()),
                writable: false,
                overridden_by_instance: false,
            });
        }
    }
    if env_file_has(&home.join(".env"), reference) {
        return Ok(CredentialInfo {
            configured: true,
            source: Some("user-env".to_string()),
            writable: false,
            overridden_by_instance: false,
        });
    }
    Ok(CredentialInfo::unconfigured())
}

// ---------------------------------------------------------------------------
// Line-level splice
// ---------------------------------------------------------------------------

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// Key of a `key: value` line; `None` for blanks, comments and sequence items.
fn key_of(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("- ") {
        return None;
    }
    let (key, _) = trimmed.split_once(':')?;
    Some(key.trim().to_string())
}

fn inline_value(line: &str) -> String {
    match line.trim().split_once(':') {
        Some((_, rest)) => rest.trim().to_string(),
        None => String::new(),
    }
}

#[derive(Clone, Copy, Debug)]
struct Section {
    key_line: usize,
    /// Content range (`start..end`); for an inline section it is empty.
    start: usize,
    end: usize,
    inline: bool,
}

fn locate_section(lines: &[String], key: &str) -> Option<Section> {
    for (i, line) in lines.iter().enumerate() {
        if indent_of(line) != 0 || key_of(line).as_deref() != Some(key) {
            continue;
        }
        let rest = inline_value(line);
        if rest.is_empty() {
            let mut end = lines.len();
            for (j, candidate) in lines.iter().enumerate().skip(i + 1) {
                if candidate.trim().is_empty() || indent_of(candidate) > 0 {
                    continue;
                }
                end = j;
                break;
            }
            let mut trimmed_end = end;
            while trimmed_end > i + 1 && lines[trimmed_end - 1].trim().is_empty() {
                trimmed_end -= 1;
            }
            return Some(Section {
                key_line: i,
                start: i + 1,
                end: trimmed_end,
                inline: false,
            });
        }
        return Some(Section {
            key_line: i,
            start: i,
            end: i,
            inline: true,
        });
    }
    None
}

/// Range of one entry inside a block section, including nested lines.
fn entry_range(lines: &[String], section: &Section, key: &str) -> Option<(usize, usize)> {
    if section.inline || section.start >= section.end {
        return None;
    }
    let mut item_indent = usize::MAX;
    for line in &lines[section.start..section.end] {
        if line.trim().is_empty() {
            continue;
        }
        item_indent = item_indent.min(indent_of(line));
    }
    if item_indent == usize::MAX {
        return None;
    }
    for i in section.start..section.end {
        if indent_of(&lines[i]) != item_indent || key_of(&lines[i]).as_deref() != Some(key) {
            continue;
        }
        let mut end = section.end;
        for (j, candidate) in lines.iter().enumerate().take(section.end).skip(i + 1) {
            if candidate.trim().is_empty() || indent_of(candidate) > item_indent {
                continue;
            }
            end = j;
            break;
        }
        return Some((i, end));
    }
    None
}

fn quote_yaml(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn render_entry(indent: usize, reference: &str, value: &str) -> Vec<String> {
    vec![format!(
        "{}{}: {}",
        " ".repeat(indent),
        reference,
        quote_yaml(value)
    )]
}

/// Entry indent used inside `refs:`; falls back to two spaces when the section
/// is still empty.
fn entry_indent(lines: &[String], section: &Section) -> usize {
    if section.inline {
        return 2;
    }
    lines[section.start..section.end]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| indent_of(l))
        .min()
        .unwrap_or(2)
        .max(1)
}

fn new_store(reference: &str, value: &str) -> String {
    format!("version: 1\nrefs:\n  {reference}: {}\n", quote_yaml(value))
}

/// Produces the document with `refs.<reference>` set, leaving everything else
/// untouched.
fn splice_set(raw: &str, reference: &str, value: &str) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Ok(new_store(reference, value));
    }
    let mut lines: Vec<String> = raw.lines().map(|l| l.to_string()).collect();
    let trailing_newline = raw.ends_with('\n');

    let Some(section) = locate_section(&lines, "refs") else {
        let at = insertion_index(&lines);
        let mut block = vec!["refs:".to_string()];
        block.extend(render_entry(2, reference, value));
        lines.splice(at..at, block);
        return join_lines(lines, trailing_newline);
    };

    if section.inline {
        let rest = inline_value(&lines[section.key_line]);
        if rest != "{}" {
            return Err(
                ".credentials.yaml 的 refs 段是内联写法，启动器无法安全改写，请改为分行写法"
                    .to_string(),
            );
        }
        let indent = entry_indent(&lines, &section);
        let mut block = vec!["refs:".to_string()];
        block.extend(render_entry(indent, reference, value));
        lines.splice(section.key_line..=section.key_line, block);
        return join_lines(lines, trailing_newline);
    }

    let indent = entry_indent(&lines, &section);
    match entry_range(&lines, &section, reference) {
        Some((start, end)) => {
            lines.splice(start..end, render_entry(indent, reference, value));
        }
        None => {
            let at = section.end;
            let block = render_entry(indent, reference, value);
            lines.splice(at..at, block);
        }
    }
    join_lines(lines, trailing_newline)
}

/// Produces the document with `refs.<reference>` removed.
fn splice_unset(raw: &str, reference: &str) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Ok(raw.to_string());
    }
    let mut lines: Vec<String> = raw.lines().map(|l| l.to_string()).collect();
    let trailing_newline = raw.ends_with('\n');
    let Some(section) = locate_section(&lines, "refs") else {
        return Ok(raw.to_string());
    };
    let Some((start, end)) = entry_range(&lines, &section, reference) else {
        return Ok(raw.to_string());
    };
    lines.drain(start..end);
    join_lines(lines, trailing_newline)
}

/// Where a missing `refs:` section belongs: before `records:`, else after
/// `version:`, else before the first top-level key, else at the end.
fn insertion_index(lines: &[String]) -> usize {
    if let Some(records) = locate_section(lines, "records") {
        return records.key_line;
    }
    if let Some(version) = locate_section(lines, "version") {
        return version.end;
    }
    for (i, line) in lines.iter().enumerate() {
        if indent_of(line) == 0 && key_of(line).is_some() {
            return i;
        }
    }
    lines.len()
}

fn join_lines(lines: Vec<String>, trailing_newline: bool) -> Result<String, String> {
    let mut out = lines.join("\n");
    if trailing_newline {
        out.push('\n');
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

fn write_store(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    // Never tmp+rename: `.credentials.yaml` is in the storage-redirect
    // whitelist and may be a hard link, which an atomic replace would break
    // (see `links.rs`).
    private_write(path, text).map_err(|e| format!("写入 {CREDENTIALS_FILENAME} 失败: {e}"))
}

#[cfg(unix)]
fn private_write(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(text.as_bytes())
}

#[cfg(not(unix))]
fn private_write(path: &Path, text: &str) -> std::io::Result<()> {
    std::fs::write(path, text)
}

/// Stores one secret. Fails before touching the file whenever the document is
/// not a store the launcher may splice.
pub fn set(path: &Path, reference: &str, value: &str) -> Result<(), String> {
    validate_reference(reference)?;
    validate_secret(value)?;
    let raw = read_optional(path)?.unwrap_or_default();
    if !raw.trim().is_empty() {
        validate_store(&raw, path)?;
    }
    let next = splice_set(&raw, reference, value)?;
    write_store(path, &next)
}

/// Removes one secret; absent files and absent references are no-ops.
pub fn unset(path: &Path, reference: &str) -> Result<(), String> {
    validate_reference(reference)?;
    let Some(raw) = read_optional(path)? else {
        return Ok(());
    };
    if !raw.trim().is_empty() {
        validate_store(&raw, path)?;
    }
    let next = splice_unset(&raw, reference)?;
    if next == raw {
        return Ok(());
    }
    write_store(path, &next)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# 由 DSH 管理，请勿手改
version: 1
records:
  client-connection/browser-session:
    kind: grant
    payload:
      version: 1
refs:
  DEEPSEEK_API_KEY: sk-existing
  OPENAI_API_KEY: sk-openai
"#;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dsh-launcher-cred-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn describe_reads_file_layer() {
        let home = temp_dir("describe-file");
        std::fs::write(store_path(&home), SAMPLE).unwrap();
        let info = describe("DEEPSEEK_API_KEY", &BTreeMap::new(), &home, None).unwrap();
        assert_eq!(
            info,
            CredentialInfo {
                configured: true,
                source: Some("file".to_string()),
                writable: true,
                overridden_by_instance: false,
            }
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn describe_env_layer_wins_and_is_read_only() {
        let home = temp_dir("describe-env");
        std::fs::write(store_path(&home), SAMPLE).unwrap();
        let mut env = BTreeMap::new();
        env.insert("DEEPSEEK_API_KEY".to_string(), "sk-from-env".to_string());
        let info = describe("DEEPSEEK_API_KEY", &env, &home, None).unwrap();
        assert_eq!(info.source.as_deref(), Some("env"));
        assert!(!info.writable);
        assert!(info.overridden_by_instance);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn empty_value_counts_as_unconfigured() {
        let home = temp_dir("describe-empty");
        std::fs::write(store_path(&home), "version: 1\nrefs:\n  KEY: ''\n").unwrap();
        let info = describe("KEY", &BTreeMap::new(), &home, None).unwrap();
        assert!(!info.configured);
        assert!(info.writable);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn describe_reads_profile_env_layer() {
        let home = temp_dir("describe-project-env");
        std::fs::write(store_path(&home), "version: 1\n").unwrap();
        let profile = home.join("profiles").join("web");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join(".env"), "# comment\nGATEWAY_API_KEY=sk-gw\n").unwrap();
        let info = describe("GATEWAY_API_KEY", &BTreeMap::new(), &home, Some("web")).unwrap();
        assert_eq!(info.source.as_deref(), Some("project-env"));
        assert!(!info.writable);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn write_replaces_in_place_keeps_records_and_comments() {
        let home = temp_dir("replace");
        let path = store_path(&home);
        std::fs::write(&path, SAMPLE).unwrap();
        set(&path, "DEEPSEEK_API_KEY", "sk-new").unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("# 由 DSH 管理，请勿手改"));
        assert!(out.contains("DEEPSEEK_API_KEY: 'sk-new'"));
        assert!(out.contains("OPENAI_API_KEY: sk-openai"));
        assert!(out.contains("client-connection/browser-session:"));
        assert!(out.contains("kind: grant"));
        let unchanged: Vec<&str> = out
            .lines()
            .filter(|l| !l.contains("DEEPSEEK_API_KEY"))
            .collect();
        let expected: Vec<&str> = SAMPLE
            .lines()
            .filter(|l| !l.contains("DEEPSEEK_API_KEY"))
            .collect();
        assert_eq!(unchanged, expected);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn write_inserts_when_ref_absent() {
        let home = temp_dir("insert");
        let path = store_path(&home);
        std::fs::write(&path, SAMPLE).unwrap();
        set(&path, "GATEWAY_API_KEY", "sk-gw").unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("  GATEWAY_API_KEY: 'sk-gw'\n"));
        assert!(out.contains("OPENAI_API_KEY: sk-openai"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn write_creates_refs_section_when_missing() {
        let home = temp_dir("no-refs");
        let path = store_path(&home);
        std::fs::write(&path, "version: 1\nrecords:\n  a/b:\n    kind: grant\n").unwrap();
        set(&path, "X_API_KEY", "sk-x").unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            out,
            "version: 1\nrefs:\n  X_API_KEY: 'sk-x'\nrecords:\n  a/b:\n    kind: grant\n"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn write_creates_file_when_missing() {
        let home = temp_dir("new-file");
        let path = store_path(&home);
        set(&path, "X_API_KEY", "sk-x").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "version: 1\nrefs:\n  X_API_KEY: 'sk-x'\n"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unset_removes_only_that_ref() {
        let home = temp_dir("unset");
        let path = store_path(&home);
        std::fs::write(&path, SAMPLE).unwrap();
        unset(&path, "DEEPSEEK_API_KEY").unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(!out.contains("DEEPSEEK_API_KEY"));
        assert!(out.contains("OPENAI_API_KEY: sk-openai"));
        assert!(out.contains("kind: grant"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unset_on_a_missing_file_is_a_noop() {
        let home = temp_dir("unset-missing");
        unset(&store_path(&home), "X_API_KEY").unwrap();
        assert!(!store_path(&home).exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn rejects_unknown_top_level_key() {
        let home = temp_dir("unknown-key");
        let path = store_path(&home);
        std::fs::write(&path, "version: 1\nrefs: {}\nextra: 1\n").unwrap();
        let err = set(&path, "X_API_KEY", "sk-x").unwrap_err();
        assert!(err.contains("未知顶层字段 extra"), "{err}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn rejects_non_v1_version() {
        let home = temp_dir("bad-version");
        let path = store_path(&home);
        std::fs::write(&path, "version: 2\nrefs: {}\n").unwrap();
        let err = set(&path, "X_API_KEY", "sk-x").unwrap_err();
        assert!(err.contains("version 不是 1"), "{err}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn rejects_missing_version() {
        let home = temp_dir("no-version");
        let path = store_path(&home);
        std::fs::write(&path, "refs:\n  X_API_KEY: sk-x\n").unwrap();
        let err = set(&path, "X_API_KEY", "sk-y").unwrap_err();
        assert!(err.contains("缺少 version: 1"), "{err}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn rejects_inline_refs_with_entries() {
        let home = temp_dir("inline-refs");
        let path = store_path(&home);
        std::fs::write(&path, "version: 1\nrefs: { A: b }\n").unwrap();
        let err = set(&path, "X_API_KEY", "sk-x").unwrap_err();
        assert!(err.contains("内联写法"), "{err}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn expands_empty_inline_refs() {
        let home = temp_dir("empty-inline");
        let path = store_path(&home);
        std::fs::write(&path, "version: 1\nrefs: {}\n").unwrap();
        set(&path, "X_API_KEY", "sk-x").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "version: 1\nrefs:\n  X_API_KEY: 'sk-x'\n"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn quoting_is_safe_for_awkward_values() {
        let home = temp_dir("quoting");
        let path = store_path(&home);
        set(&path, "X_API_KEY", "sk:a#b'c\"d").unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&out).unwrap();
        assert_eq!(
            parsed
                .get("refs")
                .and_then(|r| r.get("X_API_KEY"))
                .and_then(|v| v.as_str()),
            Some("sk:a#b'c\"d")
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn set_is_rejected_while_env_shadows() {
        let info = CredentialInfo {
            configured: true,
            source: Some("env".to_string()),
            writable: false,
            overridden_by_instance: true,
        };
        assert!(ensure_writable(&info).is_err());
        let writable = CredentialInfo {
            configured: true,
            source: Some("file".to_string()),
            writable: true,
            overridden_by_instance: false,
        };
        assert!(ensure_writable(&writable).is_ok());
    }

    #[test]
    fn key_grammar_rejects_spaces_and_control_characters() {
        assert!(is_valid_secret("sk-abc123"));
        assert!(!is_valid_secret("sk abc"));
        assert!(!is_valid_secret("sk\tabc"));
        assert!(!is_valid_secret(""));
        assert!(!is_valid_secret("sk-\u{7f}"));
    }

    #[test]
    fn ref_grammar_rejects_bad_names() {
        assert!(is_valid_reference("DEEPSEEK_API_KEY"));
        assert!(is_valid_reference("_X1"));
        assert!(!is_valid_reference("1ABC"));
        assert!(!is_valid_reference("A-B"));
        assert!(!is_valid_reference(""));
        assert!(!is_valid_reference("A B"));
    }

    #[test]
    fn nested_entry_replacement_keeps_the_block_intact() {
        let raw = "version: 1\nrefs:\n  A: |\n    multi\n    line\n  B: b\n";
        let out = splice_set(raw, "A", "x").unwrap();
        assert_eq!(out, "version: 1\nrefs:\n  A: 'x'\n  B: b\n");
    }
}
