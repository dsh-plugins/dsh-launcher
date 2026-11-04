use cap_std::fs::{Dir, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

pub(crate) const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Directory-relative capability operations prevent a symlink replacement
/// between validation and opening from granting access outside the root.
pub(crate) struct Workspace {
    dir: Dir,
}

impl Workspace {
    pub fn new(root: &Path) -> Result<Self, String> {
        if !root.is_absolute() {
            return Err("workspace must be an absolute path".into());
        }
        let dir = Dir::open_ambient_dir(root, cap_std::ambient_authority())
            .map_err(|e| format!("open selected workspace: {e}"))?;
        Ok(Self { dir })
    }

    fn validate(&self, value: &str, extension: &str, creating: bool) -> Result<PathBuf, String> {
        // Reject portable Windows separators, drive/ADS syntax and ambiguous
        // names even when this server happens to run on Unix.
        if value.is_empty() || value.len() > 1024 || value.contains(['\\', ':', '\0']) {
            return Err("path must be a short workspace-relative path using '/' separators".into());
        }
        if value.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part.ends_with([' ', '.'])
                || part.chars().any(char::is_control)
                || windows_reserved(part)
        }) {
            return Err("empty, dot, traversal, reserved or ambiguous path component".into());
        }
        let path = Path::new(value);
        if !path.components().all(|c| matches!(c, Component::Normal(_)))
            || path.extension().and_then(|s| s.to_str()) != Some(extension)
        {
            return Err(format!("expected a relative .{extension} path"));
        }
        let mut prefix = PathBuf::new();
        let components: Vec<_> = path.components().collect();
        for (index, component) in components.iter().enumerate() {
            prefix.push(component.as_os_str());
            match self.dir.symlink_metadata(&prefix) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() {
                        return Err("symlink paths are not permitted".into());
                    }
                    if index + 1 < components.len() && !metadata.is_dir() {
                        return Err("parent is not a directory".into());
                    }
                    if index + 1 == components.len() && !metadata.is_file() {
                        return Err("path is not a regular file".into());
                    }
                }
                Err(error)
                    if creating
                        && index + 1 == components.len()
                        && error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("inspect workspace-relative path: {error}")),
            }
        }
        Ok(path.to_path_buf())
    }

    pub fn read(&self, value: &str, extension: &str) -> Result<Vec<u8>, String> {
        let path = self.validate(value, extension, false)?;
        let file = self
            .dir
            .open(path)
            .map_err(|e| format!("open document: {e}"))?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            return Err("document is not a regular file of at most 16 MiB".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("document exceeds 16 MiB".into());
        }
        Ok(bytes)
    }

    pub fn create(&self, value: &str, extension: &str, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err("document exceeds 16 MiB".into());
        }
        let path = self.validate(value, extension, true)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = self
            .dir
            .open_with(&path, &options)
            .map_err(|e| format!("create document (overwriting is never allowed): {e}"))?;
        // The entire package has been validated and assembled before touching
        // disk. A failed write may leave a partial file, which is reported and
        // never silently overwritten or removed using a potentially raced path.
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("document write failed; a partial file may remain: {e}"))
    }
}

fn windows_reserved(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || (stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}
