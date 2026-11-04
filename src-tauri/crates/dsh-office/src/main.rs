use std::path::PathBuf;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let flag = args.next();
    if flag.as_deref() == Some(std::ffi::OsStr::new("--help")) && args.next().is_none() {
        println!("Usage: dsh-office --workspace ABSOLUTE_DIR\n\nRust-only MCP stdio server. The workspace must already exist.\nDOCX paragraph text and XLSX typed cells only; no formula evaluation,\nrendering, macros, external relationships or full-fidelity editing.");
        return;
    }
    let root = args.next().map(PathBuf::from);
    if flag.as_deref() != Some(std::ffi::OsStr::new("--workspace"))
        || root.is_none()
        || args.next().is_some()
    {
        eprintln!("Usage: dsh-office --workspace ABSOLUTE_DIR");
        std::process::exit(2);
    }
    if let Err(error) = dsh_office::run(&root.unwrap()) {
        eprintln!("dsh-office: {error}");
        std::process::exit(1);
    }
}
