//! TUI working-directory input parsing (issue #97).
//!
//! A TUI instance can start in a user-chosen directory instead of DSH_HOME so
//! sessions are grouped per project and project-level config (AGENTS.md,
//! .dsh/skills) is discovered from the launch directory.
//!
//! The stored value follows the same convention as DshHome.path:
//!
//! - **local instance**: a plain Windows absolute path, validated with
//!   Path::is_dir() and used directly as the PTY cwd;
//! - **WSL instance**: a Linux path *inside its distro* (e.g. /home/u/proj),
//!   validated with wsl::wsl_test(distro, "-d", path) and cd-ed into directly
//!   at launch.
//!
//! Keeping those two contracts explicit is what makes the launch path work:
//! wslpath is only needed to translate a Windows/UNC path the user picked at
//! save time, never at launch time. The module is deliberately pure so the
//! rules are covered by unit tests without a live distro.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// Parsed working-directory input: what still needs validating, and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TuiCwdTarget {
    /// No working directory configured: DSH_HOME.
    Default,
    /// Local instance: Windows absolute path, validate with is_dir().
    Local(PathBuf),
    /// WSL instance: Linux path inside the distro, validate with wsl_test -d.
    Wsl(String),
    /// WSL instance: a Windows path that needs wslpath -u before it can be
    /// validated and stored (it becomes a /mnt/<drive>/... Linux path).
    WslWindows(String),
}

/// Parses the raw working-directory input of an instance.
///
/// * raw - the value from the settings control (None / blank means DSH_HOME);
/// * distro - the instance's WSL distro, None for a local instance;
/// * cwd - the launcher's working directory, used to make a hand-typed
///   relative path absolute (local instances only).
///
/// Returns the value to validate; validation itself is the caller's job
/// because it differs per target (is_dir vs wsl_test).
pub fn parse_tui_cwd(
    raw: Option<&str>,
    distro: Option<&str>,
    cwd: &Path,
) -> Result<TuiCwdTarget, String> {
    let Some(raw) = raw else {
        return Ok(TuiCwdTarget::Default);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(TuiCwdTarget::Default);
    }
    // A pasted verbatim (\\?\C:\...) path must never reach the config: it
    // names the same directory under a different string, which would split the
    // dsh-tui session bucket and leak into the success toast (previous round).
    let cleaned = strip_verbatim_prefix(trimmed);
    let value = cleaned.trim();
    match distro {
        Some(distro) => parse_wsl(value, distro),
        None => parse_local(value, cwd),
    }
}

/// Resolves the Linux working directory a WSL TUI session must start in.
///
/// The persisted contract is a Linux path inside distro; a
/// \\wsl$\<same distro>\... spelling is accepted as an equivalent. Anything
/// else is an explicit error rather than a silent fallback to DSH_HOME:
/// launching somewhere the user did not configure is exactly the bug this
/// feature must not have.
pub fn wsl_launch_cwd(raw: &str, distro: &str) -> Result<String, String> {
    let value = raw.trim();
    let value = strip_verbatim_prefix(value);
    let value = value.trim();
    if let Some((unc_distro, linux)) = parse_wsl_unc(value) {
        if !unc_distro.eq_ignore_ascii_case(distro) {
            return Err(format!(
                "工作目录 {raw} 属于 WSL 发行版 {unc_distro}，当前实例使用 {distro}"
            ));
        }
        return normalize_linux_path(&linux);
    }
    if value.starts_with('/') {
        return normalize_linux_path(value);
    }
    Err(format!(
        "无法在发行版 {distro} 中使用工作目录 {raw}：请改为填写该发行版内的 Linux 路径（如 /home/<user>/<project>），或清空以使用 DSH_HOME"
    ))
}

/// Normalizes a Linux absolute path: trailing slashes are dropped ("/" stays
/// "/"), and a ".." segment is rejected so the stored value is the stable
/// string dsh-tui keys its session bucket by.
pub fn normalize_linux_path(raw: &str) -> Result<String, String> {
    let path = raw.trim();
    if path.is_empty() {
        return Err("目录不能为空".to_string());
    }
    if !path.starts_with('/') {
        return Err(format!(
            "WSL 实例需要发行版内的 Linux 绝对路径（以 / 开头）: {path}"
        ));
    }
    if path.split('/').any(|seg| seg == "..") {
        return Err(format!(
            "工作目录不支持 .. 段，请填写规范化的完整路径: {path}"
        ));
    }
    if path == "/" {
        return Ok("/".to_string());
    }
    Ok(path.trim_end_matches('/').to_string())
}

/// The working directory a copied instance may inherit from its source.
///
/// The stored value only means something in the dialect of its own HOME: a
/// local HOME holds a Windows path, a WSL HOME holds a path inside its distro.
/// A copy that lands on a HOME of the other kind must not carry the value
/// over — the clone would fail every launch on a directory its own runtime
/// cannot reach, so it falls back to the documented DSH_HOME default instead.
pub fn cwd_for_copy(
    cwd: Option<&str>,
    source_wsl: Option<&str>,
    target_wsl: Option<&str>,
) -> Option<String> {
    let cwd = cwd?;
    (source_wsl == target_wsl).then(|| cwd.to_string())
}

/// Parses \\wsl$\<distro>\<path> / \\wsl.localhost\<distro>\<path> (either
/// separator) into (distro, /linux/path).
///
/// The Tauri directory picker hands WSL directories back in exactly this
/// shape, and wslpath does not understand it (\\wsl$\Debian\home\u becomes
/// /mnt/c/wsl$/Debian/home/u), so the prefix has to be stripped here instead
/// (issue #97 review).
pub fn parse_wsl_unc(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim();
    let rest = trimmed
        .strip_prefix(r"\\")
        .or_else(|| trimmed.strip_prefix("//"))?;
    let mut parts = rest.splitn(2, ['\\', '/']);
    let server = parts.next()?.to_ascii_lowercase();
    if server != "wsl$" && server != "wsl.localhost" {
        return None;
    }
    let tail = parts.next()?;
    let mut tail_parts = tail.splitn(2, ['\\', '/']);
    let distro = tail_parts.next()?.trim();
    if distro.is_empty() {
        return None;
    }
    let linux = tail_parts.next().unwrap_or("").replace('\\', "/");
    let linux = linux.trim_matches('/');
    Some((
        distro.to_string(),
        if linux.is_empty() {
            "/".to_string()
        } else {
            format!("/{linux}")
        },
    ))
}

/// Removes the Windows verbatim prefix from a path.
///
/// A crate like dunce would do this too, but a dependency for one string rule
/// is not worth it; this only needs the two spellings Windows uses
/// (\\?\C:\... and \\?\UNC\server\share).
pub fn strip_verbatim_prefix(raw: &str) -> Cow<'_, str> {
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        return Cow::Owned(format!(r"\\{rest}"));
    }
    if let Some(rest) = raw.strip_prefix(r"\\?\") {
        return Cow::Borrowed(rest);
    }
    Cow::Borrowed(raw)
}

/// Local instance: a Windows path, made absolute against cwd when the user
/// typed a relative one.
fn parse_local(raw: &str, cwd: &Path) -> Result<TuiCwdTarget, String> {
    // On a Windows host a Linux-looking path here is almost always a WSL value
    // pasted into the wrong instance: Path::join would reinterpret /home/u as
    // <current drive>:\home\u and the "目录不存在" error would hide the real
    // mistake. On POSIX hosts a leading / is just a normal absolute path, so
    // the rule must stay Windows-only.
    if cfg!(windows) && raw.starts_with('/') && !is_windows_absolute(raw) {
        return Err(format!(
            "本实例使用 Windows 路径，不能填写 Linux 路径: {raw}（WSL 实例请改为填写发行版内路径）"
        ));
    }
    let path = Path::new(raw);
    // A Windows absolute path is absolute for this function's contract even
    // when the tests (or a non-Windows build) run on another host: joining it
    // would silently turn `C:\proj` into `<cwd>/C:\proj`.
    let absolute = if is_windows_absolute(raw) || path.is_absolute() {
        path.to_path_buf()
    } else {
        // Also covers a drive-relative root such as \project.
        cwd.join(path)
    };
    Ok(TuiCwdTarget::Local(absolute))
}

/// WSL instance: the stored value is the Linux path inside the distro.
fn parse_wsl(raw: &str, distro: &str) -> Result<TuiCwdTarget, String> {
    if let Some((unc_distro, linux)) = parse_wsl_unc(raw) {
        if !unc_distro.eq_ignore_ascii_case(distro) {
            return Err(format!(
                "该目录属于 WSL 发行版 {unc_distro}，当前实例使用 {distro}；请填写 {distro} 内的 Linux 路径（如 /home/<user>/<project>）"
            ));
        }
        return normalize_linux_path(&linux).map(TuiCwdTarget::Wsl);
    }
    if raw.starts_with('/') {
        return normalize_linux_path(raw).map(TuiCwdTarget::Wsl);
    }
    if raw.starts_with('~') {
        return Err("WSL 实例请填写以 / 开头的完整 Linux 路径（不支持 ~ 简写）".to_string());
    }
    if is_windows_absolute(raw) {
        return Ok(TuiCwdTarget::WslWindows(raw.to_string()));
    }
    Err(format!(
        "WSL 实例的工作目录必须是绝对路径：发行版内 Linux 路径（如 /home/u/proj）或 Windows 路径（如 C:\\Users\\u\\proj）: {raw}"
    ))
}

/// C:\..., C:/... or a \\server\share UNC path.
fn is_windows_absolute(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    drive || raw.starts_with(r"\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host-neutral launcher cwd: the Windows-path cases below are
    /// drive-prefixed on purpose, so the cwd only has to be a real path on
    /// whichever host runs the tests.
    fn cwd() -> PathBuf {
        std::env::temp_dir().join("dsh-tui-cwd-fixture")
    }

    #[test]
    fn blank_input_means_dsh_home() {
        for raw in [None, Some(""), Some("   "), Some("\t\n ")] {
            assert_eq!(
                parse_tui_cwd(raw, None, &cwd()).unwrap(),
                TuiCwdTarget::Default
            );
            assert_eq!(
                parse_tui_cwd(raw, Some("Debian"), &cwd()).unwrap(),
                TuiCwdTarget::Default
            );
        }
    }

    #[test]
    fn local_paths_are_absolute_and_never_verbatim() {
        assert_eq!(
            parse_tui_cwd(Some(r"C:\Users\u\proj"), None, &cwd()).unwrap(),
            TuiCwdTarget::Local(PathBuf::from(r"C:\Users\u\proj"))
        );
        // A hand-typed relative path is resolved against the launcher cwd.
        assert_eq!(
            parse_tui_cwd(Some("proj"), None, &cwd()).unwrap(),
            TuiCwdTarget::Local(cwd().join("proj"))
        );
        // A pasted verbatim path loses the prefix: it must be stored as the
        // plain path dsh-tui keys its workspace by.
        assert_eq!(
            parse_tui_cwd(Some(r"\\?\C:\Users\u\proj"), None, &cwd()).unwrap(),
            TuiCwdTarget::Local(PathBuf::from(r"C:\Users\u\proj"))
        );
        assert_eq!(strip_verbatim_prefix(r"\\?\UNC\srv\share"), r"\\srv\share");
        assert_eq!(strip_verbatim_prefix(r"C:\plain"), r"C:\plain");
    }

    /// Windows only: a POSIX host's own local paths legitimately start with
    /// "/", so the "that is a WSL path" guard must not fire there.
    #[cfg(windows)]
    #[test]
    fn local_instance_rejects_a_linux_path() {
        let err = parse_tui_cwd(Some("/home/u/proj"), None, &cwd()).unwrap_err();
        assert!(err.contains("Windows 路径"), "unexpected error: {err}");
    }

    #[test]
    fn copied_cwd_survives_only_within_the_same_runtime() {
        // Reusing the source HOME keeps the value, in either runtime.
        assert_eq!(
            cwd_for_copy(Some(r"D:\proj"), None, None),
            Some(r"D:\proj".to_string())
        );
        assert_eq!(
            cwd_for_copy(Some("/home/u/proj"), Some("Debian"), Some("Debian")),
            Some("/home/u/proj".to_string())
        );
        // A clone that gets a HOME of the other runtime must not inherit a
        // path its own runtime cannot reach.
        assert_eq!(
            cwd_for_copy(Some("/home/u/proj"), Some("Debian"), None),
            None
        );
        assert_eq!(cwd_for_copy(Some(r"D:\proj"), None, Some("Debian")), None);
        // Nothing configured stays nothing.
        assert_eq!(cwd_for_copy(None, None, None), None);
    }

    #[test]
    fn wsl_linux_paths_are_stored_as_written() {
        assert_eq!(
            parse_tui_cwd(Some("/home/u/proj"), Some("Debian"), &cwd()).unwrap(),
            TuiCwdTarget::Wsl("/home/u/proj".to_string())
        );
        // Trailing slashes are dropped so the value is the stable bucket key.
        assert_eq!(
            parse_tui_cwd(Some("  /home/u/proj/  "), Some("Debian"), &cwd()).unwrap(),
            TuiCwdTarget::Wsl("/home/u/proj".to_string())
        );
        assert_eq!(
            parse_tui_cwd(Some("/"), Some("Debian"), &cwd()).unwrap(),
            TuiCwdTarget::Wsl("/".to_string())
        );
        // ".." would make the stored string drift from the real directory.
        assert!(parse_tui_cwd(Some("/home/u/../proj"), Some("Debian"), &cwd()).is_err());
        // "~" is not expanded by "test -d"; refuse it with a clear message.
        let err = parse_tui_cwd(Some("~/proj"), Some("Debian"), &cwd()).unwrap_err();
        assert!(err.contains('~'), "unexpected error: {err}");
    }

    #[test]
    fn wsl_unc_paths_map_to_distro_paths() {
        assert_eq!(
            parse_tui_cwd(Some(r"\\wsl$\Debian\home\u\proj"), Some("Debian"), &cwd()).unwrap(),
            TuiCwdTarget::Wsl("/home/u/proj".to_string())
        );
        assert_eq!(
            parse_tui_cwd(
                Some(r"\\wsl.localhost\Debian\home\u\proj"),
                Some("Debian"),
                &cwd()
            )
            .unwrap(),
            TuiCwdTarget::Wsl("/home/u/proj".to_string())
        );
        // Forward slashes and case differences are the same share.
        assert_eq!(
            parse_tui_cwd(Some("//WSL$/debian/home/u/proj"), Some("Debian"), &cwd()).unwrap(),
            TuiCwdTarget::Wsl("/home/u/proj".to_string())
        );
        assert_eq!(
            parse_wsl_unc(r"\\wsl$\Debian"),
            Some(("Debian".to_string(), "/".to_string()))
        );
        assert_eq!(parse_wsl_unc(r"\\server\share"), None);

        // A directory from another distro cannot be reached with cd.
        let err =
            parse_tui_cwd(Some(r"\\wsl$\Ubuntu\home\u\proj"), Some("Debian"), &cwd()).unwrap_err();
        assert!(err.contains("Ubuntu"), "unexpected error: {err}");
        assert!(err.contains("Debian"), "unexpected error: {err}");
    }

    #[test]
    fn wsl_windows_paths_need_wslpath() {
        assert_eq!(
            parse_tui_cwd(Some(r"D:\Projects\repos"), Some("Debian"), &cwd()).unwrap(),
            TuiCwdTarget::WslWindows(r"D:\Projects\repos".to_string())
        );
        assert_eq!(
            parse_tui_cwd(Some("D:/Projects/repos"), Some("Debian"), &cwd()).unwrap(),
            TuiCwdTarget::WslWindows("D:/Projects/repos".to_string())
        );
        // Paths with spaces must survive parsing intact.
        assert_eq!(
            parse_tui_cwd(
                Some(r"C:\Users\Administrator\my project"),
                Some("Debian"),
                &cwd()
            )
            .unwrap(),
            TuiCwdTarget::WslWindows(r"C:\Users\Administrator\my project".to_string())
        );
        assert!(parse_tui_cwd(Some("relative/proj"), Some("Debian"), &cwd()).is_err());
        assert!(parse_tui_cwd(Some(r"\\server\share"), Some("Debian"), &cwd()).is_ok());
    }

    #[test]
    fn wsl_launch_cwd_accepts_the_stored_contract() {
        assert_eq!(
            wsl_launch_cwd("/home/u/proj/", "Debian").unwrap(),
            "/home/u/proj"
        );
        assert_eq!(
            wsl_launch_cwd(r"\\wsl$\Debian\home\u\proj", "Debian").unwrap(),
            "/home/u/proj"
        );
        // A Windows path or another distro's share is an explicit failure, not
        // a silent DSH_HOME fallback (issue #97 review, blocker 3).
        let err = wsl_launch_cwd(r"C:\Users\u\proj", "Debian").unwrap_err();
        assert!(err.contains("Linux 路径"), "unexpected error: {err}");
        let err = wsl_launch_cwd(r"\\wsl$\Ubuntu\home\u", "Debian").unwrap_err();
        assert!(err.contains("Ubuntu"), "unexpected error: {err}");
    }
}
