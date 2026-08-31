//! 验证电源断言真的被系统接受：持有 15 秒，期间用
//! `pmset -g assertions | grep RCTool` 应能看到一条 PreventUserIdleSystemSleep。
//!
//! 跑法：cargo run -p rctool-core --example power_probe [断言名]
//!
//! 传个非 ASCII 的名字进去能复现一个坑：断言照样生效，但 pmset 把名字显示成
//! 空串。所以生产代码里的断言名一律用 ASCII。

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    #[cfg(target_os = "macos")]
    {
        let guard = rctool_core::power::KeepAwake::hold(&std::env::args().nth(1).unwrap_or_else(|| "RCTool power_probe".into()));
        if guard.is_none() {
            eprintln!("申请断言失败");
            std::process::exit(1);
        }
        println!("断言已持有 15 秒，现在可以跑：pmset -g assertions | grep -i rctool");
        std::thread::sleep(std::time::Duration::from_secs(15));
        drop(guard);
        println!("已归还。再跑一次 pmset 应该就查不到了。");
    }

    #[cfg(not(target_os = "macos"))]
    println!("电源断言仅 macOS 实现");
}
