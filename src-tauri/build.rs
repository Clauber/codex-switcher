fn main() {
    println!("cargo:rustc-check-cfg=cfg(desktop)");
    println!("cargo:rustc-check-cfg=cfg(mobile)");
    #[cfg(feature = "desktop-app")]
    tauri_build::build()
}
