//! 打印当前前台应用与可选应用列表——按应用映射填 bundle id 时的排查工具。
//!
//! `cargo run -p rctool-core --example frontapp_probe`

fn main() {
    #[cfg(target_os = "macos")]
    {
        use rctool_core::frontapp;
        match frontapp::frontmost() {
            Some(app) => println!("前台：{} ({})", app.name, app.bundle_id),
            None => println!("前台：无（或该进程没有 bundle id）"),
        }
        println!("\n可选应用（activationPolicy = regular）：");
        for app in frontapp::running_apps() {
            println!("  {:<28} {}", app.bundle_id, app.name);
        }
    }
    #[cfg(not(target_os = "macos"))]
    println!("仅 macOS 可用");
}
