//! 键盘事件探针：用一个只读的 CGEventTap 把系统里每一次按键都打出来，包括
//! 键码、修饰键，以及 `kCGEventSourceUserData`（[`rctool_core::hidmap`] 给自己
//! 注入的事件盖的那枚合成标记）。
//!
//! 和 [`report_probe`] 配对使用，用来切开「读不到」与「注入了但被吃掉」：
//!
//! - report_probe 有 `0xF1`、这里没有 keycode 53 → 读到了但没注入出来
//! - 两边都有 → 注入成功，问题在接收方（前台是谁、有没有别的 tap 拦截）
//! - report_probe 也没有 → 压根没读到报文
//!
//! 需要「辅助功能」权限（创建事件 tap 所需）；没有权限时 CGEventTapCreate
//! 返回 NULL，本探针会直接说明并退出，不会静默假装在监听。
//!
//! 运行：cargo run -p rctool-core --example key_tap_probe

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("key_tap_probe 只在 macOS 上有意义");
}

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(target_os = "macos")]
mod macos {
    use core_foundation_sys::base::{kCFAllocatorDefault, CFRelease, CFTypeRef};
    use core_foundation_sys::runloop::{
        kCFRunLoopCommonModes, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRun,
        CFRunLoopSourceRef,
    };
    use std::ffi::c_void;

    type CFMachPortRef = *mut c_void;
    type CGEventRef = *mut c_void;
    type CGEventTapProxy = *mut c_void;

    type CGEventTapCallBack =
        extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            events_of_interest: u64,
            callback: CGEventTapCallBack,
            user_info: *mut c_void,
        ) -> CFMachPortRef;
        fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
        fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
        fn CGEventGetFlags(event: CGEventRef) -> u64;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFMachPortCreateRunLoopSource(
            allocator: *const c_void,
            port: CFMachPortRef,
            order: isize,
        ) -> CFRunLoopSourceRef;
    }

    const SESSION_EVENT_TAP: u32 = 1; // kCGSessionEventTap
    const HEAD_INSERT: u32 = 0; // kCGHeadInsertEventTap
    const LISTEN_ONLY: u32 = 1; // kCGEventTapOptionListenOnly
    const EVENT_KEY_DOWN: u32 = 10;
    const EVENT_KEY_UP: u32 = 11;
    const FIELD_KEYCODE: u32 = 9; // kCGKeyboardEventKeycode
    const FIELD_USER_DATA: u32 = 42; // kCGEventSourceUserData

    /// 与 hidmap 里注入时盖的标记保持一致，用来认出「这是 RCTool 自己发的」。
    const SYNTHETIC_MARKER: i64 = 0x5243_5401; // "RCT\x01"

    pub fn run() {
        unsafe {
            let mask = (1u64 << EVENT_KEY_DOWN) | (1u64 << EVENT_KEY_UP);
            let tap = CGEventTapCreate(
                SESSION_EVENT_TAP,
                HEAD_INSERT,
                LISTEN_ONLY,
                mask,
                tap_callback,
                std::ptr::null_mut(),
            );
            if tap.is_null() {
                println!(
                    "CGEventTapCreate 返回 NULL —— 当前进程没有「辅助功能」权限，无法监听按键。\n\
                     去 系统设置 → 隐私与安全性 → 辅助功能 给运行本探针的终端授权后重试。"
                );
                return;
            }
            let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0);
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
            CFRelease(source as CFTypeRef);
            CGEventTapEnable(tap, true);

            println!("监听所有按键事件；按遥控器或键盘，Ctrl-C 退出。\n");
            CFRunLoopRun();
        }
    }

    extern "C" fn tap_callback(
        _proxy: CGEventTapProxy,
        etype: u32,
        event: CGEventRef,
        _user_info: *mut c_void,
    ) -> CGEventRef {
        if etype == EVENT_KEY_DOWN || etype == EVENT_KEY_UP {
            let keycode = unsafe { CGEventGetIntegerValueField(event, FIELD_KEYCODE) };
            let marker = unsafe { CGEventGetIntegerValueField(event, FIELD_USER_DATA) };
            let flags = unsafe { CGEventGetFlags(event) };
            let phase = if etype == EVENT_KEY_DOWN { "按下" } else { "松开" };
            let origin = if marker == SYNTHETIC_MARKER {
                "RCTool 注入 ✅"
            } else {
                "非 RCTool（真实按键或其他来源）"
            };
            println!(
                "{phase}  keycode={keycode:<4}{}  flags={flags:#011x}  来源={origin}",
                name_of(keycode)
            );
        }
        event
    }

    /// 只标注排查里会用到的几个键，其余留空即可。
    fn name_of(keycode: i64) -> &'static str {
        match keycode {
            53 => " (Escape)",
            51 => " (Delete/退格)",
            36 => " (Return)",
            48 => " (Tab)",
            123 => " (←)",
            124 => " (→)",
            125 => " (↓)",
            126 => " (↑)",
            103 => " (F11)",
            109 => " (F10)",
            50 => " (`)",
            _ => "",
        }
    }
}
