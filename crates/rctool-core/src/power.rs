//! macOS 电源断言：遥控器在场期间阻止系统进入 idle sleep。
//!
//! 用 `kIOPMAssertionTypePreventUserIdleSystemSleep`（等价于 `caffeinate -i`）。
//! 它只压住**空闲计时器**触发的睡眠——用户从菜单主动选「睡眠」、按电源键、
//! 合盖仍然照睡，所以不存在"把机器卡在醒着"的情况。
//!
//! 断言是 RAII 的：[`KeepAwake`] 一旦 drop 就归还，进程异常退出时由内核回收。
//! 因此上层只要持有/丢弃这个句柄，不需要自己配对 create/release。

#![cfg(target_os = "macos")]

use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::string::CFStringRef;

type IOReturn = i32;
type IOPMAssertionID = u32;
type IOPMAssertionLevel = u32;

const IO_RETURN_SUCCESS: IOReturn = 0;
const ASSERTION_LEVEL_ON: IOPMAssertionLevel = 255;

/// `kIOPMAssertionTypePreventUserIdleSystemSleep` 的字面值。IOKit 把它导出为
/// CFSTR 常量，跨 FFI 拿常量指针不如直接构造字符串省事，值本身是稳定 ABI。
const PREVENT_USER_IDLE_SYSTEM_SLEEP: &str = "PreventUserIdleSystemSleep";

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: CFStringRef,
        assertion_level: IOPMAssertionLevel,
        assertion_name: CFStringRef,
        assertion_id: *mut IOPMAssertionID,
    ) -> IOReturn;
    fn IOPMAssertionRelease(assertion_id: IOPMAssertionID) -> IOReturn;
}

/// 持有期间系统不会 idle sleep。丢弃即归还。
#[derive(Debug)]
pub struct KeepAwake {
    id: IOPMAssertionID,
}

impl KeepAwake {
    /// 申请断言。`reason` 会出现在 `pmset -g assertions` 里，方便用户查证是谁
    /// 按住了睡眠——失败返回 `None`（调用方按"没拿到"处理即可，不是致命错误）。
    ///
    /// **`reason` 必须是 ASCII。** 实测 `pmset` 遇到非 ASCII 的断言名会渲染成
    /// 空串（断言本身照常生效，只是名字丢了），而这个名字正是用户排查"谁不让
    /// 我的 Mac 睡觉"的唯一线索——丢了就等于没有。
    pub fn hold(reason: &str) -> Option<KeepAwake> {
        let kind = CFString::new(PREVENT_USER_IDLE_SYSTEM_SLEEP);
        let name = CFString::new(reason);
        let mut id: IOPMAssertionID = 0;
        let rc = unsafe {
            IOPMAssertionCreateWithName(
                kind.as_concrete_TypeRef(),
                ASSERTION_LEVEL_ON,
                name.as_concrete_TypeRef(),
                &mut id,
            )
        };
        if rc == IO_RETURN_SUCCESS {
            log::info!("已持有防睡眠断言（{reason}）");
            Some(KeepAwake { id })
        } else {
            log::warn!("申请防睡眠断言失败: IOReturn={rc:#010X}");
            None
        }
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        unsafe { IOPMAssertionRelease(self.id) };
        log::info!("已归还防睡眠断言");
    }
}
