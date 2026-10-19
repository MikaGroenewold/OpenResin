fn main() {
    let mut attributes = tauri_build::Attributes::new();
    // tauri needs Common Controls v6, declared in the application manifest. The
    // manifest normally comes with the Windows resource file, which is only
    // linked into binaries, so the test executables could not start
    // (STATUS_ENTRYPOINT_NOT_FOUND). With MSVC the linker embeds the same
    // manifest into every executable instead (app, unit and integration tests).
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        attributes = attributes.windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("windows-app-manifest.xml");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    }
    tauri_build::try_build(attributes).expect("tauri-build failed");
}
