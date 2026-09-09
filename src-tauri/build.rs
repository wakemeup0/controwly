fn main() {
    println!("cargo:rerun-if-changed=windows/app.manifest");
    let attributes = tauri_build::Attributes::new().windows_attributes(
        tauri_build::WindowsAttributes::new().app_manifest(include_str!("windows/app.manifest")),
    );
    tauri_build::try_build(attributes).expect("failed to configure Tauri build");
}
