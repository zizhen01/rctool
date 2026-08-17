fn main() {
    tauri_build::build();
    // macOS：CoreBluetooth 需要蓝牙用途声明；Tauri 打包会用 Info.plist，
    // 但 `tauri dev` 直接跑二进制，这里补一段嵌入（与 CLI 同理）。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = format!("{}/Info.dev.plist", std::env::var("CARGO_MANIFEST_DIR").unwrap());
        if std::path::Path::new(&plist).exists() {
            println!("cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,{plist}");
            println!("cargo:rerun-if-changed=Info.dev.plist");
        }
    }
}
