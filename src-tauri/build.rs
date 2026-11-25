fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rerun-if-changed=src/native_browser/macos.m");
        // Intel 版本也支持尚无 Network.framework 的 macOS 10.13。
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,Network");
        let minimum = if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
            "11.0"
        } else {
            "10.13"
        };
        cc::Build::new()
            .env("MACOSX_DEPLOYMENT_TARGET", minimum)
            .file("src/native_browser/macos.m")
            .flag("-fobjc-arc")
            .flag("-fblocks")
            .compile("launcher_browser_macos");
    }
    let attributes = tauri_build::Attributes::new().plugin(
        "sidebar-browser",
        tauri_build::InlinedPlugin::new().commands(&["send"]),
    );
    tauri_build::try_build(attributes).expect("生成桌面启动器资源失败")
}
