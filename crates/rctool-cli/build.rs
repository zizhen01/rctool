fn main() {
    // macOS：裸二进制访问 CoreBluetooth 必须携带 NSBluetoothAlwaysUsageDescription，
    // 否则进程会被 TCC 直接 SIGABRT。把 Info.plist 嵌进 __TEXT,__info_plist 段。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = format!("{}/Info.plist", std::env::var("CARGO_MANIFEST_DIR").unwrap());
        println!("cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,{plist}");
        println!("cargo:rerun-if-changed=Info.plist");
    }
}
