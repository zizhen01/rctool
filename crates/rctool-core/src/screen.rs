//! macOS 锁屏状态查询与解锁键入。
//!
//! 只做两件事：问「现在锁着吗」，以及「把这串字符敲进登录窗」。什么时候该敲
//! 由上层（在场监视器）决定。
//!
//! 为什么是轮询而不是监听 `com.apple.screenIsLocked` 分布式通知：在场监视器
//! 本来就在按秒轮询，[`is_locked`] 是一次进程内的 session 字典查询，成本可以
//! 忽略。顺带绕开了通知路径的一个坑——只监听"锁屏事件"会漏掉应用启动时屏幕
//! 已经锁着的情况，而轮询天然覆盖。
//!
//! 安全提醒：合成键盘事件把密码打进登录窗，与 Apple 的 Auto Unlock 完全不是
//! 一回事。能仿冒被绑定遥控器的人就能解开这台机器。上层必须让它默认关闭。

#![cfg(target_os = "macos")]

use core_foundation::base::TCFType;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::base::{CFGetTypeID, CFRelease, CFTypeRef};
use core_foundation_sys::dictionary::CFDictionaryRef;
use core_foundation_sys::number::{
    CFBooleanGetTypeID, CFBooleanGetValue, CFBooleanRef, CFNumberGetTypeID, CFNumberRef,
};
use std::ffi::c_void;

type CGEventSourceRef = *mut c_void;
type CGEventRef = *mut c_void;

/// `kCGEventSourceStateHIDSystemState`：合成的事件走 HID 层，登录窗才收得到。
const EVENT_SOURCE_HID_SYSTEM_STATE: u32 = 1;
/// `kCGHIDEventTap`：投递点同样必须是 HID tap。
const HID_EVENT_TAP: u32 = 0;

/// 承载 Unicode 串的虚拟键码。设了 unicode string 后键码本身不参与解释，
/// 取值沿用 BLEUnlock 长期出货验证过的常量。
const VK_UNICODE_CARRIER: u16 = 49;
/// 回车。同样沿用 BLEUnlock 的取值——它在登录窗上是验证过能提交的。
const VK_RETURN: u16 = 52;

/// 单个键盘事件最多携带的 UTF-16 码元数。超过这个长度系统会截断。
const CHARS_PER_EVENT: usize = 20;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceCreate(state_id: u32) -> CGEventSourceRef;
    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        keycode: u16,
        key_down: bool,
    ) -> CGEventRef;
    fn CGEventKeyboardSetUnicodeString(
        event: CGEventRef,
        string_length: isize,
        unicode_string: *const u16,
    );
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
}

/// 当前登录会话的屏幕是否锁着。
///
/// 读 `CGSessionCopyCurrentDictionary()` 里的 `CGSSessionScreenIsLocked`。取不到
/// 字典（例如没有图形会话）时按"没锁"处理——宁可不动作，也不要在无法判断的
/// 情况下往未知焦点里敲密码。
pub fn is_locked() -> bool {
    unsafe {
        let raw = CGSessionCopyCurrentDictionary();
        if raw.is_null() {
            return false;
        }
        let dict: CFDictionary<CFString, CFTypeRef> =
            CFDictionary::wrap_under_create_rule(raw);
        let key = CFString::new("CGSSessionScreenIsLocked");
        let Some(value) = dict.find(&key) else {
            return false;
        };
        // 实测这个键是 CFBoolean，但没有文档保证，历史上也见过 CFNumber 的说法。
        // 两种都认，都不是就按"没锁"——见上面的保守原则。
        let type_id = CFGetTypeID(*value);
        if type_id == CFBooleanGetTypeID() {
            CFBooleanGetValue(*value as CFBooleanRef)
        } else if type_id == CFNumberGetTypeID() {
            CFNumber::wrap_under_get_rule(*value as CFNumberRef).to_i64().unwrap_or(0) == 1
        } else {
            false
        }
    }
}

/// 把 `text` 敲进当前焦点，然后回车提交。
///
/// 调用方必须自己确认：屏幕确实锁着、确实该解锁。本函数不做任何判断——它只
/// 负责敲，敲到哪里取决于调用时的焦点。需要辅助功能权限，否则事件被系统丢弃
/// （静默失败，不报错）。
pub fn type_and_submit(text: &str) {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        let source = CGEventSourceCreate(EVENT_SOURCE_HID_SYSTEM_STATE);
        for chunk in utf16.chunks(CHARS_PER_EVENT) {
            let down = CGEventCreateKeyboardEvent(source, VK_UNICODE_CARRIER, true);
            CGEventKeyboardSetUnicodeString(down, chunk.len() as isize, chunk.as_ptr());
            CGEventPost(HID_EVENT_TAP, down);
            CFRelease(down as CFTypeRef);

            let up = CGEventCreateKeyboardEvent(source, VK_UNICODE_CARRIER, false);
            CGEventPost(HID_EVENT_TAP, up);
            CFRelease(up as CFTypeRef);
        }

        let down = CGEventCreateKeyboardEvent(source, VK_RETURN, true);
        CGEventPost(HID_EVENT_TAP, down);
        CFRelease(down as CFTypeRef);
        let up = CGEventCreateKeyboardEvent(source, VK_RETURN, false);
        CGEventPost(HID_EVENT_TAP, up);
        CFRelease(up as CFTypeRef);

        if !source.is_null() {
            CFRelease(source as CFTypeRef);
        }
    }
}
