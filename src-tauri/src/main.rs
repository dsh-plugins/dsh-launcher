// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Dispatch before the GUI, logging, deep-link and single-instance plugins.
    // MCP owns stdout exclusively; it must also work while the GUI is running.
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(std::ffi::OsStr::new("--office-mcp")) {
        let flag = args.next();
        let root = args.next();
        if flag.as_deref() != Some(std::ffi::OsStr::new("--workspace"))
            || root.is_none()
            || args.next().is_some()
        {
            eprintln!("Usage: dsh-launcher --office-mcp --workspace ABSOLUTE_DIRECTORY");
            std::process::exit(2);
        }
        let root = std::path::PathBuf::from(root.unwrap());
        if let Err(error) = dsh_office::run(&root) {
            eprintln!("Office MCP: {error}");
            std::process::exit(1);
        }
        return;
    }
    dsh_launcher_lib::run()
}
