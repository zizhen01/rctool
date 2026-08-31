//! macOS 登录密码的钥匙串存取（仅自动解锁功能使用）。
//!
//! 密码只进钥匙串，**不进配置文件、不进日志、不进任何 DTO**。界面只能问
//! [`has_password`]「存了没有」，拿不到明文；后端也只在真要敲进登录窗的那一刻
//! 才 [`get_password`]。
//!
//! 条目是当前用户默认钥匙串里的一条泛型密码，account 固定为登录用户名——同一台
//! 机器上换用户就是另一条，不会串。

#![cfg(target_os = "macos")]

use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

/// 钥匙串条目的 service 名。用户在「钥匙串访问」里能按这个名字找到并手动删除。
const SERVICE: &str = "RCTool 自动解锁";

fn account() -> String {
    std::env::var("USER").unwrap_or_else(|_| "default".into())
}

pub fn set_password(password: &str) -> Result<(), String> {
    // set_generic_password 是 upsert 语义，已存在会覆盖，不必先删。
    set_generic_password(SERVICE, &account(), password.as_bytes())
        .map_err(|e| format!("写入钥匙串失败: {e}"))
}

pub fn get_password() -> Option<String> {
    match get_generic_password(SERVICE, &account()) {
        Ok(bytes) => String::from_utf8(bytes).ok(),
        Err(e) => {
            // 不打印错误细节以外的任何东西——这条路径离密码明文只有一步。
            log::debug!("读取钥匙串条目失败: {e}");
            None
        }
    }
}

pub fn has_password() -> bool {
    get_generic_password(SERVICE, &account()).is_ok()
}

pub fn clear_password() -> Result<(), String> {
    match delete_generic_password(SERVICE, &account()) {
        Ok(()) => Ok(()),
        // 本来就没有 == 已经是想要的状态。
        Err(e) if !has_password() => {
            log::debug!("删除钥匙串条目: {e}（条目本就不存在）");
            Ok(())
        }
        Err(e) => Err(format!("删除钥匙串条目失败: {e}")),
    }
}
