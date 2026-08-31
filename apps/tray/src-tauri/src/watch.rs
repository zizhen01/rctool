//! 在场监管：把「遥控器在不在」翻译成防睡眠断言与自动解锁动作。
//!
//! 两条独立的任务：
//!
//! - **监视任务**跑 [`rctool_core::presence::watch`]，把在场状态写进一个原子标志。
//!   它按 5s 轮询系统连接表，翻转有 90s 宽限，节奏慢而稳。
//! - **监管任务**按 2s 读那个标志，据此持有/归还电源断言、必要时执行解锁。
//!   它比监视任务快，是因为锁屏是随时可能发生的事件（远程会话断开、触发角），
//!   而在场状态本身变得很慢。
//!
//! 两者共用一个 [`CancellationToken`]：配置一改就整体重启，不做增量更新——
//! 这类监管逻辑的状态机分支远比重建成本贵。

use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

use rctool_core::presence::{self, Presence, PresenceOptions};

/// 监管任务的节拍。锁屏后最坏要等这么久才开始解锁。
const SUPERVISE_TICK: Duration = Duration::from_secs(2);
/// 同一次锁屏最多尝试几次。密码错了就不该无限重试——那等于拿登录窗当爆破靶子，
/// 也会把用户的错误密码次数刷满。
const MAX_UNLOCK_ATTEMPTS: u32 = 3;
/// 两次尝试之间的间隔，留给登录窗处理上一次输入。
const UNLOCK_RETRY_DELAY: Duration = Duration::from_secs(5);

/// 当前生效的监管设置。取自配置的快照——监管任务不回头读配置，
/// 配置变了由上层重启任务。
#[derive(Debug, Clone)]
pub struct Settings {
    pub bound_id: Option<String>,
    pub keep_awake: bool,
    pub auto_unlock: bool,
}

#[derive(Clone, Serialize)]
pub struct PresenceDto {
    pub present: bool,
    /// 此刻是否真的握着防睡眠断言（开了开关但遥控器不在时为 false）。
    pub keeping_awake: bool,
}

/// 只握一个取消令牌——任务本身是分离的。
///
/// 为什么不 join：重启要在同步的配置锁里完成，await 会把锁跨过 await 点。
/// 取消后旧监管任务最迟一拍（[`SUPERVISE_TICK`]）就收尾并归还断言，这期间
/// 新旧断言并存完全无害——IOPM 断言本来就是引用计数的。
pub struct Handle {
    token: CancellationToken,
}

impl Handle {
    /// 取消并放手。旧任务自行收尾。
    pub fn cancel(self) {
        self.token.cancel();
    }
}

/// 平台相关的副作用。非 macOS 上全是空操作，好让监管循环本身保持一份代码。
#[derive(Default)]
struct Effects {
    #[cfg(target_os = "macos")]
    awake: Option<rctool_core::power::KeepAwake>,
}

impl Effects {
    fn holding(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.awake.is_some()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    fn set_keep_awake(&mut self, _wanted: bool) {
        #[cfg(target_os = "macos")]
        {
            match (_wanted, self.awake.is_some()) {
                (true, false) => {
                    // 必须 ASCII——pmset 显示不了非 ASCII 断言名，见 KeepAwake::hold 的说明。
                    self.awake =
                        rctool_core::power::KeepAwake::hold("RCTool: remote is nearby");
                }
                (false, true) => self.awake = None, // Drop 即归还
                _ => {}
            }
        }
    }

    /// 屏幕锁着吗。非 macOS 恒为 false（没实现，也就不会触发解锁）。
    fn screen_locked(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            rctool_core::screen::is_locked()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    /// 真正把密码敲出去。返回是否敲了。
    fn try_unlock(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            if !rctool_core::hidmap::Permissions::query().accessibility {
                log::warn!("自动解锁需要辅助功能权限，当前未授予——本次跳过");
                return false;
            }
            let Some(password) = crate::keychain::get_password() else {
                log::warn!("自动解锁已开启，但钥匙串里没有密码——本次跳过");
                return false;
            };
            log::info!("屏幕锁着且遥控器在场，执行自动解锁");
            rctool_core::screen::type_and_submit(&password);
            true
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }
}

/// 启动监视 + 监管。两条任务是分离的；对返回的句柄调 [`Handle::cancel`]
/// 即让它们各自收尾——监管任务收尾时丢弃 [`Effects`]，断言随之归还。
pub fn start(app: AppHandle, settings: Settings) -> Handle {
    let token = CancellationToken::new();
    let present = Arc::new(AtomicBool::new(false));

    {
        let token = token.clone();
        let present = present.clone();
        let app = app.clone();
        let opts = PresenceOptions { bound_id: settings.bound_id.clone(), ..Default::default() };
        tauri::async_runtime::spawn(async move {
            let cb: presence::PresenceCallback = Arc::new(move |p: Presence| {
                present.store(p == Presence::Present, Ordering::Relaxed);
                // 立刻把在场变化推给界面；是否握着断言由监管任务下一拍补正。
                let _ = app.emit(
                    "presence-status",
                    PresenceDto { present: p == Presence::Present, keeping_awake: false },
                );
            });
            if let Err(e) = presence::watch(&opts, &token, cb).await {
                log::warn!("在场检测停止: {e:#}");
            }
        });
    }

    {
        let token = token.clone();
        let present = present.clone();
        tauri::async_runtime::spawn(async move {
            let mut effects = Effects::default();
            let mut attempts: u32 = 0;
            let mut last_attempt: Option<Instant> = None;
            let mut last_reported: Option<PresenceDto> = None;

            loop {
                let here = present.load(Ordering::Relaxed);
                effects.set_keep_awake(settings.keep_awake && here);

                if effects.screen_locked() {
                    let cooled = last_attempt
                        .map(|t| t.elapsed() >= UNLOCK_RETRY_DELAY)
                        .unwrap_or(true);
                    if settings.auto_unlock && here && attempts < MAX_UNLOCK_ATTEMPTS && cooled {
                        if effects.try_unlock() {
                            attempts += 1;
                            last_attempt = Some(Instant::now());
                        } else {
                            // 权限或密码缺失：这次锁屏期间别再刷日志。
                            attempts = MAX_UNLOCK_ATTEMPTS;
                        }
                    }
                } else {
                    // 屏幕开着就重置——下一次锁屏是全新的一轮。
                    attempts = 0;
                    last_attempt = None;
                }

                let dto = PresenceDto { present: here, keeping_awake: effects.holding() };
                let changed = last_reported
                    .as_ref()
                    .map(|p| p.present != dto.present || p.keeping_awake != dto.keeping_awake)
                    .unwrap_or(true);
                if changed {
                    let _ = app.emit("presence-status", dto.clone());
                    last_reported = Some(dto);
                }

                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = tokio::time::sleep(SUPERVISE_TICK) => {}
                }
            }
            // 显式丢弃：任务结束时归还断言，不留给进程退出兜底。
            drop(effects);
        });
    }

    Handle { token }
}
