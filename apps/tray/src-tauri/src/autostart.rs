//! macOS 开机自启：`~/Library/LaunchAgents/dev.rctool.tray.plist`。
//!
//! 为什么写 LaunchAgent 而不是走 AppleScript 登录项：后者要「自动化」权限，
//! 在无头机上没人点授权框就静默失败。LaunchAgent 是纯文件操作，不需要任何
//! 额外授权，且在「系统设置 > 通用 > 登录项 > 允许在后台」里照常可见可关。
//!
//! 只写 `RunAtLoad`，不写 `KeepAlive`：加了 KeepAlive 用户就退不掉应用了
//! （一退就被拉起来），代价大于收益。
//!
//! 状态的唯一真相是这个 plist 文件本身，不进配置文件——两处各存一份迟早不一致。

#![cfg(target_os = "macos")]

use std::path::PathBuf;

const LABEL: &str = "dev.rctool.tray";

fn plist_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

/// 当前可执行文件路径。LaunchAgent 直接拉起 .app 里的这个二进制——它仍在
/// bundle 内，所以 TCC 认的还是 RCTool 的签名身份。
fn program() -> Option<String> {
    std::env::current_exe().ok()?.to_str().map(str::to_string)
}

fn plist_body(program: &str) -> String {
    // 路径进 XML 前先转义。macOS 路径里出现 & < > 很少见但不是不可能。
    let escaped = program
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{escaped}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#
    )
}

pub fn is_enabled() -> bool {
    plist_path().map(|p| p.exists()).unwrap_or(false)
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let path = plist_path().ok_or_else(|| "找不到 HOME 目录".to_string())?;
    if enabled {
        let program = program().ok_or_else(|| "取不到当前可执行文件路径".to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("创建 LaunchAgents 目录失败: {e}"))?;
        }
        std::fs::write(&path, plist_body(&program))
            .map_err(|e| format!("写入 LaunchAgent 失败: {e}"))?;
        log::info!("已启用开机自启: {}", path.display());
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => log::info!("已关闭开机自启"),
            // 本来就没有 == 已经是想要的状态。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("删除 LaunchAgent 失败: {e}")),
        }
    }
    Ok(())
}

/// 应用换了位置（重装、从别处拷过来）后，plist 里记的还是旧路径，开机就拉不起来。
/// 启动时对一次，不一致就按当前路径重写。已关闭自启则什么都不做。
pub fn refresh_path_if_enabled() {
    if !is_enabled() {
        return;
    }
    let (Some(path), Some(program)) = (plist_path(), program()) else { return };
    let wanted = plist_body(&program);
    match std::fs::read_to_string(&path) {
        Ok(current) if current == wanted => {}
        _ => {
            if let Err(e) = std::fs::write(&path, wanted) {
                log::warn!("更新 LaunchAgent 路径失败: {e}");
            } else {
                log::info!("应用位置有变，已更新开机自启路径: {program}");
            }
        }
    }
}
