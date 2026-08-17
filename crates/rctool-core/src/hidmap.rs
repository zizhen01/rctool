//! macOS 按键映射：非独占 HID 读取 + 关联式原生事件拦截 + 键盘注入。
//!
//! 为什么不独占（seize）：独占会让系统不再处理这台设备，连带废掉
//! [`crate::fnmap`] 的 F5→Fn 听写触发。所以走监听模式——系统照常处理设备，
//! 听写继续可用；只对被改键的按钮，用 CGEventTap 按 usage 时序关联抵消其
//! 原生事件，同时注入映射动作。返回键(0xF1)是系统层死键，不产生原生事件，
//! 直接读出注入即可，无需拦截。
//!
//! 线程模型：一个专用线程持有自己的 CFRunLoop，IOHIDManager 与事件 tap 的
//! 回调都在该线程上串行触发。keymap 更新经 `Arc<Mutex>` 跨线程写入。所有
//! CoreFoundation/CoreGraphics 句柄只在该线程创建与使用。

#![cfg(target_os = "macos")]

use crate::keymap::{Disposition, Injection, KeyMap, Mods, RemoteButton};
use core_foundation::base::TCFType;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation_sys::base::{kCFAllocatorDefault, CFRelease, CFTypeRef};
use core_foundation_sys::runloop::{
    kCFRunLoopCommonModes, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRef, CFRunLoopRun,
    CFRunLoopSourceRef, CFRunLoopStop,
};
use core_foundation_sys::string::CFStringRef;
use std::cell::{Cell, RefCell, UnsafeCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// FFI
// ---------------------------------------------------------------------------

type CGEventSourceRef = *mut c_void;
type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CFMachPortRef = *mut c_void;
type IOHIDManagerRef = *mut c_void;
type IOHIDDeviceRef = *mut c_void;
type IOReturn = i32;

type CGEventTapCallBack = extern "C" fn(
    proxy: CGEventTapProxy,
    etype: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;
type IOHIDDeviceCallback =
    extern "C" fn(context: *mut c_void, result: IOReturn, sender: *mut c_void, device: IOHIDDeviceRef);
type IOHIDReportCallback = extern "C" fn(
    context: *mut c_void,
    result: IOReturn,
    sender: *mut c_void,
    rtype: u32,
    report_id: u32,
    report: *mut u8,
    report_length: isize,
);

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventSourceCreate(state_id: u32) -> CGEventSourceRef;
    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        keycode: u16,
        key_down: bool,
    ) -> CGEventRef;
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDManagerCreate(allocator: *const c_void, options: u32) -> IOHIDManagerRef;
    fn IOHIDManagerSetDeviceMatching(manager: IOHIDManagerRef, matching: *const c_void);
    fn IOHIDManagerRegisterDeviceMatchingCallback(
        manager: IOHIDManagerRef,
        callback: IOHIDDeviceCallback,
        context: *mut c_void,
    );
    fn IOHIDManagerRegisterDeviceRemovalCallback(
        manager: IOHIDManagerRef,
        callback: IOHIDDeviceCallback,
        context: *mut c_void,
    );
    fn IOHIDManagerScheduleWithRunLoop(
        manager: IOHIDManagerRef,
        run_loop: CFRunLoopRef,
        run_loop_mode: CFStringRef,
    );
    fn IOHIDManagerUnscheduleFromRunLoop(
        manager: IOHIDManagerRef,
        run_loop: CFRunLoopRef,
        run_loop_mode: CFStringRef,
    );
    fn IOHIDManagerOpen(manager: IOHIDManagerRef, options: u32) -> IOReturn;
    fn IOHIDManagerClose(manager: IOHIDManagerRef, options: u32) -> IOReturn;
    fn IOHIDDeviceRegisterInputReportCallback(
        device: IOHIDDeviceRef,
        report: *mut u8,
        report_length: isize,
        callback: IOHIDReportCallback,
        context: *mut c_void,
    );
    fn IOHIDCheckAccess(request_type: u32) -> u32;
    fn IOHIDRequestAccess(request_type: u32) -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

// CoreGraphics 常量
const CG_EVENT_SOURCE_HID: u32 = 1; // kCGEventSourceStateHIDSystemState
const CG_HID_EVENT_TAP: u32 = 0; // kCGHIDEventTap（注入位置）
const CG_SESSION_EVENT_TAP: u32 = 1; // kCGSessionEventTap（拦截位置）
const CG_HEAD_INSERT: u32 = 0; // kCGHeadInsertEventTap
const CG_TAP_OPTION_DEFAULT: u32 = 0;
const CG_FIELD_KEYCODE: u32 = 9; // kCGKeyboardEventKeycode
const CG_FIELD_USER_DATA: u32 = 42; // kCGEventSourceUserData
const EVT_KEY_DOWN: u32 = 10;
const EVT_KEY_UP: u32 = 11;
const EVT_TAP_DISABLED_TIMEOUT: u32 = 0xFFFF_FFFE;
const EVT_TAP_DISABLED_USER_INPUT: u32 = 0xFFFF_FFFF;

const FLAG_SHIFT: u64 = 1 << 17;
const FLAG_CONTROL: u64 = 1 << 18;
const FLAG_OPTION: u64 = 1 << 19;
const FLAG_COMMAND: u64 = 1 << 20;
const FLAG_FN: u64 = 1 << 23;

// IOKit 常量
const IOHID_OPTIONS_NONE: u32 = 0;
const IOHID_REQUEST_LISTEN_EVENT: u32 = 1; // kIOHIDRequestTypeListenEvent
const IOHID_ACCESS_GRANTED: u32 = 0; // kIOHIDAccessTypeGranted

/// 注入事件的来源标记，供拦截回调识别"这是我自己发的"从而放行。
const SYNTHETIC_MARKER: i64 = 0x5243_5401; // "RCT\x01"
/// 关联拦截的时间窗：按下遥控器键后，这段时间内到达的同码原生事件被抵消。
const ARM_TTL: Duration = Duration::from_millis(220);

fn cg_flags(mods: Mods) -> u64 {
    let mut f = 0;
    if mods.contains(Mods::COMMAND) {
        f |= FLAG_COMMAND;
    }
    if mods.contains(Mods::SHIFT) {
        f |= FLAG_SHIFT;
    }
    if mods.contains(Mods::CONTROL) {
        f |= FLAG_CONTROL;
    }
    if mods.contains(Mods::OPTION) {
        f |= FLAG_OPTION;
    }
    if mods.contains(Mods::FN) {
        f |= FLAG_FN;
    }
    f
}

// ---------------------------------------------------------------------------
// 权限
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    /// 输入监控：读取 HID 报文所需。
    pub input_monitoring: bool,
    /// 辅助功能：注入事件与创建拦截 tap 所需。
    pub accessibility: bool,
}

impl Permissions {
    pub fn query() -> Permissions {
        unsafe {
            Permissions {
                input_monitoring: IOHIDCheckAccess(IOHID_REQUEST_LISTEN_EVENT)
                    == IOHID_ACCESS_GRANTED,
                accessibility: AXIsProcessTrusted(),
            }
        }
    }

    /// 弹出输入监控授权请求（系统对话框）。
    pub fn request_input_monitoring() -> bool {
        unsafe { IOHIDRequestAccess(IOHID_REQUEST_LISTEN_EVENT) }
    }

    pub fn ready(self) -> bool {
        self.input_monitoring && self.accessibility
    }
}

// ---------------------------------------------------------------------------
// 注入器
// ---------------------------------------------------------------------------

struct Injector {
    source: CGEventSourceRef,
}

impl Injector {
    fn new() -> Option<Injector> {
        let source = unsafe { CGEventSourceCreate(CG_EVENT_SOURCE_HID) };
        (!source.is_null()).then_some(Injector { source })
    }

    fn emit(&self, inj: Injection, down: bool) {
        unsafe {
            let event = CGEventCreateKeyboardEvent(self.source, inj.keycode, down);
            if event.is_null() {
                return;
            }
            CGEventSetFlags(event, cg_flags(inj.mods));
            CGEventSetIntegerValueField(event, CG_FIELD_USER_DATA, SYNTHETIC_MARKER);
            CGEventPost(CG_HID_EVENT_TAP, event);
            CFRelease(event as CFTypeRef);
        }
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        if !self.source.is_null() {
            unsafe { CFRelease(self.source as CFTypeRef) };
        }
    }
}

// ---------------------------------------------------------------------------
// 共享状态（跨线程）
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Armed {
    keycode: u16,
    down: bool,
    expiry: Instant,
}

struct Shared {
    keymap: KeyMap,
    armed: Vec<Armed>,
}

impl Shared {
    fn arm(&mut self, keycode: u16, down: bool) {
        let now = Instant::now();
        self.armed.retain(|a| a.expiry > now);
        self.armed.push(Armed { keycode, down, expiry: now + ARM_TTL });
        if self.armed.len() > 64 {
            let overflow = self.armed.len() - 64;
            self.armed.drain(0..overflow);
        }
    }

    /// 消费一个匹配的拦截条目，返回是否命中（命中即应抵消该原生事件）。
    fn take_match(&mut self, keycode: u16, down: bool) -> bool {
        let now = Instant::now();
        self.armed.retain(|a| a.expiry > now);
        if let Some(i) =
            self.armed.iter().position(|a| a.keycode == keycode && a.down == down)
        {
            self.armed.remove(i);
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// 读取器上下文（只在读取线程使用）
// ---------------------------------------------------------------------------

struct ReaderCtx {
    shared: Arc<Mutex<Shared>>,
    injector: Injector,
    /// 当前按下的 usage 集合（用于计算按下/松开边沿）。
    active_usages: RefCell<HashSet<u16>>,
    /// 已注入并保持按下的映射（松开时补 keyUp）。
    held: RefCell<HashMap<RemoteButton, Injection>>,
    /// IOHIDDeviceRegisterInputReportCallback 需要一个常驻缓冲区。
    report_buf: UnsafeCell<[u8; 64]>,
    active_device: Cell<IOHIDDeviceRef>,
}

impl ReaderCtx {
    fn handle_report(&self, report_id: u32, data: &[u8]) {
        let Some(usages) = crate::keymap::parse_report_usages(report_id, data) else {
            return;
        };
        let new: HashSet<u16> = usages.into_iter().collect();
        let mut active = self.active_usages.borrow_mut();
        let pressed: Vec<u16> = new.difference(&active).copied().collect();
        let released: Vec<u16> = active.difference(&new).copied().collect();
        *active = new;
        drop(active);

        for usage in pressed {
            if let Some(button) = RemoteButton::from_hid_usage(usage) {
                self.on_edge(button, true);
            }
        }
        for usage in released {
            if let Some(button) = RemoteButton::from_hid_usage(usage) {
                self.on_edge(button, false);
            }
        }
    }

    fn on_edge(&self, button: RemoteButton, down: bool) {
        let disposition = {
            let shared = self.shared.lock().unwrap();
            shared.keymap.disposition(button)
        };
        match disposition {
            Disposition::Passthrough => {}
            Disposition::Suppress => {
                // 拦截原生行为、不注入。仅当该键有原生事件可抵消时才布防。
                if let Some(native) = button.native() {
                    self.shared.lock().unwrap().arm(native.keycode, down);
                }
            }
            Disposition::Remap(inj) => {
                // 抵消原生（若有），并注入映射键；保持按下语义。
                if let Some(native) = button.native() {
                    self.shared.lock().unwrap().arm(native.keycode, down);
                }
                if down {
                    self.injector.emit(inj, true);
                    self.held.borrow_mut().insert(button, inj);
                } else if let Some(inj) = self.held.borrow_mut().remove(&button) {
                    self.injector.emit(inj, false);
                }
            }
        }
    }

    /// 断开或停止时释放仍按住的注入键，避免修饰键卡住。
    fn release_all_held(&self) {
        for (_, inj) in self.held.borrow_mut().drain() {
            self.injector.emit(inj, false);
        }
        self.active_usages.borrow_mut().clear();
    }
}

// 回调里对 ReaderCtx 的访问都发生在读取线程，天然串行。
extern "C" fn match_callback(
    context: *mut c_void,
    _result: IOReturn,
    _sender: *mut c_void,
    device: IOHIDDeviceRef,
) {
    let ctx = unsafe { &*(context as *const ReaderCtx) };
    if !ctx.active_device.get().is_null() {
        log::warn!("已在读取一台遥控器，忽略第二台匹配设备");
        return;
    }
    ctx.active_device.set(device);
    let buf = ctx.report_buf.get() as *mut u8;
    unsafe {
        IOHIDDeviceRegisterInputReportCallback(
            device,
            buf,
            64,
            report_callback,
            context,
        );
    }
    log::info!("RC003 HID 按键设备已就绪");
}

extern "C" fn removal_callback(
    context: *mut c_void,
    _result: IOReturn,
    _sender: *mut c_void,
    device: IOHIDDeviceRef,
) {
    let ctx = unsafe { &*(context as *const ReaderCtx) };
    if ctx.active_device.get() == device {
        ctx.release_all_held();
        ctx.active_device.set(std::ptr::null_mut());
        log::info!("RC003 HID 按键设备已断开");
    }
}

extern "C" fn report_callback(
    context: *mut c_void,
    result: IOReturn,
    _sender: *mut c_void,
    _rtype: u32,
    report_id: u32,
    report: *mut u8,
    report_length: isize,
) {
    if result != 0 || report.is_null() || report_length <= 0 {
        return;
    }
    let ctx = unsafe { &*(context as *const ReaderCtx) };
    let data = unsafe { std::slice::from_raw_parts(report, report_length as usize) };
    ctx.handle_report(report_id, data);
}

extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    etype: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    // tap 被系统禁用（超时/用户输入）时重新启用。
    if etype == EVT_TAP_DISABLED_TIMEOUT || etype == EVT_TAP_DISABLED_USER_INPUT {
        let shared = unsafe { &*(user_info as *const TapCtx) };
        unsafe { CGEventTapEnable(shared.tap.load(Ordering::Relaxed), true) };
        return event;
    }
    if etype != EVT_KEY_DOWN && etype != EVT_KEY_UP {
        return event;
    }
    // 放行自己注入的事件。
    let marker = unsafe { CGEventGetIntegerValueField(event, CG_FIELD_USER_DATA) };
    if marker == SYNTHETIC_MARKER {
        return event;
    }
    let keycode = unsafe { CGEventGetIntegerValueField(event, CG_FIELD_KEYCODE) } as u16;
    let down = etype == EVT_KEY_DOWN;
    let ctx = unsafe { &*(user_info as *const TapCtx) };
    let hit = ctx.shared.lock().unwrap().take_match(keycode, down);
    if hit {
        std::ptr::null_mut() // 抵消该原生事件
    } else {
        event
    }
}

/// 拦截 tap 的上下文（读取线程持有；tap 回调用其中的 shared 判断抵消）。
struct TapCtx {
    shared: Arc<Mutex<Shared>>,
    tap: AtomicPtr<c_void>,
}

// ---------------------------------------------------------------------------
// 公开句柄
// ---------------------------------------------------------------------------

/// 控制 HID 按键映射的运行。丢弃时自动停止读取线程并恢复。
pub struct HidMapper {
    shared: Arc<Mutex<Shared>>,
    run_loop: Arc<AtomicPtr<c_void>>,
    thread: Option<JoinHandle<()>>,
}

impl HidMapper {
    /// 启动读取线程。需要输入监控权限；拦截/注入需要辅助功能权限，
    /// 缺失时降级（返回键仍可读、可注入部分动作），不崩溃。
    pub fn start(keymap: KeyMap) -> HidMapper {
        let shared = Arc::new(Mutex::new(Shared { keymap, armed: Vec::new() }));
        let run_loop = Arc::new(AtomicPtr::new(std::ptr::null_mut()));
        let thread = {
            let shared = shared.clone();
            let run_loop = run_loop.clone();
            std::thread::Builder::new()
                .name("rctool-hid".into())
                .spawn(move || reader_thread(shared, run_loop))
                .expect("spawn HID 读取线程")
        };
        HidMapper { shared, run_loop, thread: Some(thread) }
    }

    /// 热更新按键映射（设置界面改键即时生效，无需重启读取线程）。
    pub fn update_keymap(&self, keymap: KeyMap) {
        self.shared.lock().unwrap().keymap = keymap;
    }
}

impl Drop for HidMapper {
    fn drop(&mut self) {
        let rl = self.run_loop.load(Ordering::Acquire);
        if !rl.is_null() {
            unsafe { CFRunLoopStop(rl as CFRunLoopRef) };
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn reader_thread(shared: Arc<Mutex<Shared>>, run_loop_slot: Arc<AtomicPtr<c_void>>) {
    unsafe {
        let run_loop = CFRunLoopGetCurrent();
        run_loop_slot.store(run_loop as *mut c_void, Ordering::Release);

        let Some(injector) = Injector::new() else {
            log::error!("无法创建事件注入源，HID 映射未启动");
            return;
        };

        // 读取器上下文（常驻本线程）。
        let ctx = Box::new(ReaderCtx {
            shared: shared.clone(),
            injector,
            active_usages: RefCell::new(HashSet::new()),
            held: RefCell::new(HashMap::new()),
            report_buf: UnsafeCell::new([0u8; 64]),
            active_device: Cell::new(std::ptr::null_mut()),
        });
        let ctx_ptr = Box::into_raw(ctx);

        // IOHIDManager：按 VID/PID 匹配遥控器（监听模式，不 seize）。
        let manager = IOHIDManagerCreate(kCFAllocatorDefault, IOHID_OPTIONS_NONE);
        if manager.is_null() {
            log::error!("无法创建 IOHIDManager");
            drop(Box::from_raw(ctx_ptr));
            return;
        }
        let matching = CFDictionary::from_CFType_pairs(&[
            (
                CFString::from_static_string("VendorID"),
                CFNumber::from(crate::fnmap::VENDOR_ID),
            ),
            (
                CFString::from_static_string("ProductID"),
                CFNumber::from(crate::fnmap::PRODUCT_ID),
            ),
        ]);
        IOHIDManagerSetDeviceMatching(manager, matching.as_CFTypeRef());
        IOHIDManagerRegisterDeviceMatchingCallback(
            manager,
            match_callback,
            ctx_ptr as *mut c_void,
        );
        IOHIDManagerRegisterDeviceRemovalCallback(
            manager,
            removal_callback,
            ctx_ptr as *mut c_void,
        );
        IOHIDManagerScheduleWithRunLoop(manager, run_loop, kCFRunLoopCommonModes);
        let open = IOHIDManagerOpen(manager, IOHID_OPTIONS_NONE);
        if open != 0 {
            log::error!("打开 IOHIDManager 失败（错误 {open}）；通常是缺少「输入监控」权限");
        }

        // 拦截 tap（会话级），失败则降级：无抵消但注入/返回键仍工作。
        let tap_ctx = Box::new(TapCtx {
            shared: shared.clone(),
            tap: AtomicPtr::new(std::ptr::null_mut()),
        });
        let tap_ctx_ptr = Box::into_raw(tap_ctx);
        let mask = (1u64 << EVT_KEY_DOWN) | (1u64 << EVT_KEY_UP);
        let tap = CGEventTapCreate(
            CG_SESSION_EVENT_TAP,
            CG_HEAD_INSERT,
            CG_TAP_OPTION_DEFAULT,
            mask,
            tap_callback,
            tap_ctx_ptr as *mut c_void,
        );
        let mut tap_source: CFRunLoopSourceRef = std::ptr::null_mut();
        if tap.is_null() {
            log::warn!("无法创建事件拦截 tap（通常缺少「辅助功能」权限）；改键的原生行为将无法抵消");
        } else {
            (*tap_ctx_ptr).tap.store(tap, Ordering::Relaxed);
            tap_source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            CFRunLoopAddSource(run_loop, tap_source, kCFRunLoopCommonModes);
            CGEventTapEnable(tap, true);
            log::info!("按键映射已启动");
        }

        // 运行事件循环，直到 Drop 调用 CFRunLoopStop。
        CFRunLoopRun();

        // 清理（仍在本线程）。
        (*ctx_ptr).release_all_held();
        IOHIDManagerUnscheduleFromRunLoop(manager, run_loop, kCFRunLoopCommonModes);
        IOHIDManagerClose(manager, IOHID_OPTIONS_NONE);
        CFRelease(manager as CFTypeRef);
        if !tap.is_null() {
            CGEventTapEnable(tap, false);
            CFRelease(tap as CFTypeRef);
            if !tap_source.is_null() {
                CFRelease(tap_source as CFTypeRef);
            }
        }
        run_loop_slot.store(std::ptr::null_mut(), Ordering::Release);
        drop(Box::from_raw(ctx_ptr));
        drop(Box::from_raw(tap_ctx_ptr));
        log::info!("按键映射已停止");
    }
}
