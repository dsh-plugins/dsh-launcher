//! Opt-in Office MCP preset. Building a preset never writes configuration.
use std::path::{Path, PathBuf};

use tauri::State;

use crate::mcp::{McpServer, McpTransport};
use crate::AppState;

fn preset(executable: &Path, workspace: &Path) -> Result<McpServer, String> {
    if !workspace.is_absolute() {
        return Err("Office workspace must be an absolute directory".into());
    }
    let workspace = workspace.canonicalize().map_err(|e| e.to_string())?;
    if !workspace.is_dir() {
        return Err("Office workspace must be an existing directory".into());
    }
    let text = |path: &Path| {
        path.to_str()
            .map(str::to_owned)
            .ok_or_else(|| "Office paths must be valid Unicode".to_string())
    };
    Ok(McpServer {
        id: String::new(),
        server_name: "office-rust".into(),
        transport: McpTransport::Stdio,
        url: String::new(),
        headers: Vec::new(),
        command: text(executable)?,
        args: vec![
            "--office-mcp".into(),
            "--workspace".into(),
            text(&workspace)?,
        ],
        env: Vec::new(),
        cwd: text(&workspace)?,
        enabled: true,
        extra: serde_json::Map::new(),
    })
}

/// Returns a reviewable form only. The normal MCP Save action persists it.
#[tauri::command]
pub async fn office_mcp_preset(
    state: State<'_, AppState>,
    home_id: String,
    workspace: String,
) -> Result<McpServer, String> {
    {
        let config = state.config.lock().unwrap();
        let home = config
            .homes
            .iter()
            .find(|home| home.id == home_id)
            .ok_or_else(|| "DSH_HOME does not exist".to_string())?;
        if home.wsl.is_some() {
            return Err("The built-in Office preset currently supports native instances only. Build dsh-office inside WSL and configure it manually.".into());
        }
    }
    // AppImage's current_exe is in an ephemeral mount. Persist its outer path.
    let executable = if cfg!(target_os = "linux") {
        std::env::var_os("APPIMAGE")
            .map(PathBuf::from)
            .unwrap_or(std::env::current_exe().map_err(|e| e.to_string())?)
    } else {
        std::env::current_exe().map_err(|e| e.to_string())?
    };
    if !executable.is_absolute() || !executable.is_file() {
        return Err("Cannot locate the Office host executable".into());
    }
    let server = preset(&executable, Path::new(&workspace))?;
    // Probe the current inner binary, not an AppImage launcher wrapper that
    // may spawn its own mount/helper processes. Persist the outer path above.
    let probe_executable = std::env::current_exe().map_err(|e| e.to_string())?;
    crate::office_probe::check(&probe_executable, &server.args, Path::new(&server.cwd)).await?;
    Ok(server)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_is_scoped_and_uses_arguments_without_shell() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let exe = root.join("launcher with spaces");
        let server = preset(&exe, &root).unwrap();
        assert_eq!(
            server.args,
            ["--office-mcp", "--workspace", root.to_str().unwrap()]
        );
        assert_eq!(server.command, exe.to_str().unwrap());
        assert_eq!(server.cwd, root.to_str().unwrap());
        assert!(server.id.is_empty());
        assert_eq!(server.transport, McpTransport::Stdio);
    }

    #[test]
    fn rejects_relative_workspace() {
        assert!(preset(Path::new("launcher"), Path::new(".")).is_err());
    }
}
