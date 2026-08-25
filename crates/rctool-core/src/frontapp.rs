//! macOS 前台应用检测（NSWorkspace）。
//!
//! 为 [`crate::keymap::AppKeyMaps`] 提供"现在是哪个 app 在前台"这一个事实：
//! 上层据此把对应的按键映射热更新给 [`crate::hidmap`]。
//!
//! 用通知而不是轮询：`NSWorkspaceDidActivateApplicationNotification` 在切换
//! 完成的瞬间到达，切走后按下的第一次按键就已经用上新映射；轮询无论多密都
//! 会留下一个"用错映射"的窗口，而且空转在电池上是纯浪费。
//!
//! 线程约束：观察者必须在主线程（NSApplication 的 runloop）注册，回调也在
//! 主线程触发。观察者一旦注册就跟随进程终身存在——没有"停止检测"的需求，
//! 换来的是调用方不必持有一个 `!Send` 的句柄。查询函数（[`frontmost`]、
//! [`running_apps`]）读的是框架维护的缓存、返回线程安全的
//! `NSRunningApplication`，任意线程可调。

#![cfg(target_os = "macos")]

use block2::RcBlock;
use objc2_app_kit::{
    NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace,
    NSWorkspaceDidActivateApplicationNotification,
};
use objc2_foundation::NSNotification;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// 一个可被识别的应用。`bundle_id` 是匹配键，`name` 只用于显示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontApp {
    pub bundle_id: String,
    pub name: String,
}

pub type Callback = Arc<dyn Fn(Option<FrontApp>) + Send + Sync + 'static>;

static CALLBACK: Mutex<Option<Callback>> = Mutex::new(None);
static INSTALLED: AtomicBool = AtomicBool::new(false);

fn describe(app: &NSRunningApplication) -> Option<FrontApp> {
    // 没有 bundle id 的进程（部分命令行程序、辅助进程）无法作为规则的匹配
    // 键，直接当作"没有前台应用"，让映射回落到全局。
    let bundle_id = app.bundleIdentifier()?.to_string();
    let name = app
        .localizedName()
        .map(|n| n.to_string())
        .unwrap_or_else(|| bundle_id.clone());
    Some(FrontApp { bundle_id, name })
}

/// 当前前台应用。无前台应用或其无 bundle id 时返回 `None`。
pub fn frontmost() -> Option<FrontApp> {
    let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
    describe(&app)
}

/// 当前正在运行、且在 Dock/切换器里可见的应用（`activationPolicy == regular`）。
///
/// 设置界面用它做选择列表——比让用户手抄 bundle id 靠谱。后台代理与系统辅助
/// 进程（accessory/prohibited）不会成为前台应用，列出来只是噪音。
pub fn running_apps() -> Vec<FrontApp> {
    let workspace = NSWorkspace::sharedWorkspace();
    let mut apps: Vec<FrontApp> = workspace
        .runningApplications()
        .iter()
        .filter(|app| {
            app.activationPolicy() == NSApplicationActivationPolicy::Regular
        })
        .filter_map(|app| describe(&app))
        .collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps.dedup_by(|a, b| a.bundle_id == b.bundle_id);
    apps
}

/// 注册前台应用变化回调。**必须在主线程调用**（Tauri 的 `setup` 即是）。
///
/// 重复调用只替换回调，不会重复注册观察者。回调在主线程触发，里面不要做
/// 阻塞的事。注册后会立即用当前前台应用回调一次，避免启动时状态为空。
pub fn watch(callback: Callback) {
    *CALLBACK.lock().unwrap() = Some(callback);
    if !INSTALLED.swap(true, Ordering::SeqCst) {
        let block = RcBlock::new(|_: NonNull<NSNotification>| {
            // 通知的 userInfo 里带着刚激活的 app，但直接问一次
            // frontmostApplication 结果相同且少一层可选解包。
            dispatch(frontmost());
        });
        unsafe {
            let center = NSWorkspace::sharedWorkspace().notificationCenter();
            let token = center.addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceDidActivateApplicationNotification),
                None,
                None,
                &block,
            );
            // 观察者终身有效：泄漏 token 换取调用方无需持有 !Send 句柄。
            std::mem::forget(token);
        }
    }
    dispatch(frontmost());
}

/// 回调期间不持锁：万一回调里又触发了一次通知（同线程重入），持锁调用会
/// 自己把自己锁死。
fn dispatch(app: Option<FrontApp>) {
    let callback = CALLBACK.lock().unwrap().clone();
    if let Some(cb) = callback {
        cb(app);
    }
}
