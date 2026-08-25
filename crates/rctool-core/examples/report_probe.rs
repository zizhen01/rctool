//! 原始 HID 报文探针：把遥控器发出的每一份 input report 原样打出来，并标注
//! 解析出的 usage 以及它是否映射到某个 [`RemoteButton`]。
//!
//! 排查「某个键按了没反应」时用它——生产路径（[`rctool_core::hidmap`]）对
//! 不认识的 usage 是静默忽略的，只有这里能看见「键确实发了，但 usage 和
//! 表里对不上」。
//!
//! 与生产路径的一个关键差异：**这里给每一台匹配 VID/PID 的设备都挂上回调**。
//! hidmap 只读第一台（其余打一行 warn 就忽略），所以如果某个键的报文来自
//! 第二台设备，生产路径永远收不到、而这里能收到——两边输出对不上本身就是
//! 结论。
//!
//! 运行：cargo run -p rctool-core --example report_probe
//! 然后逐个按遥控器上的键，Ctrl-C 退出。

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("report_probe 只在 macOS 上有意义");
}

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(target_os = "macos")]
mod macos {
    use core_foundation::base::TCFType;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_foundation_sys::base::kCFAllocatorDefault;
    use core_foundation_sys::runloop::{
        kCFRunLoopCommonModes, CFRunLoopGetCurrent, CFRunLoopRef, CFRunLoopRun,
    };
    use core_foundation_sys::string::CFStringRef;
    use rctool_core::fnmap::{PRODUCT_ID, VENDOR_ID};
    use rctool_core::keymap::{parse_report_usages, RemoteButton};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type IOHIDManagerRef = *mut c_void;
    type IOHIDDeviceRef = *mut c_void;
    type IOReturn = i32;

    type IOHIDDeviceCallback = extern "C" fn(*mut c_void, IOReturn, *mut c_void, IOHIDDeviceRef);
    type IOHIDReportCallback =
        extern "C" fn(*mut c_void, IOReturn, *mut c_void, u32, u32, *mut u8, isize);

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOHIDManagerCreate(allocator: *const c_void, options: u32) -> IOHIDManagerRef;
        fn IOHIDManagerSetDeviceMatching(manager: IOHIDManagerRef, matching: *const c_void);
        fn IOHIDManagerRegisterDeviceMatchingCallback(
            manager: IOHIDManagerRef,
            callback: IOHIDDeviceCallback,
            context: *mut c_void,
        );
        fn IOHIDManagerScheduleWithRunLoop(
            manager: IOHIDManagerRef,
            run_loop: CFRunLoopRef,
            run_loop_mode: CFStringRef,
        );
        fn IOHIDManagerOpen(manager: IOHIDManagerRef, options: u32) -> IOReturn;
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

    const REQUEST_LISTEN_EVENT: u32 = 1; // kIOHIDRequestTypeListenEvent
    const ACCESS_GRANTED: u32 = 0; // kIOHIDAccessTypeGranted

    static DEVICE_COUNT: AtomicUsize = AtomicUsize::new(0);

    pub fn run() {
        unsafe {
            if IOHIDCheckAccess(REQUEST_LISTEN_EVENT) != ACCESS_GRANTED {
                println!("未获得「输入监控」权限，正在申请……");
                IOHIDRequestAccess(REQUEST_LISTEN_EVENT);
                println!("若系统弹窗，请授权后重新运行本探针。");
            }

            let manager = IOHIDManagerCreate(kCFAllocatorDefault, 0);
            assert!(!manager.is_null(), "IOHIDManagerCreate 失败");

            let matching = CFDictionary::from_CFType_pairs(&[
                (
                    CFString::from_static_string("VendorID"),
                    CFNumber::from(VENDOR_ID),
                ),
                (
                    CFString::from_static_string("ProductID"),
                    CFNumber::from(PRODUCT_ID),
                ),
            ]);
            IOHIDManagerSetDeviceMatching(manager, matching.as_CFTypeRef());
            IOHIDManagerRegisterDeviceMatchingCallback(
                manager,
                match_callback,
                std::ptr::null_mut(),
            );
            IOHIDManagerScheduleWithRunLoop(
                manager,
                CFRunLoopGetCurrent(),
                kCFRunLoopCommonModes,
            );
            let rc = IOHIDManagerOpen(manager, 0);
            if rc != 0 {
                println!("IOHIDManagerOpen 返回 {rc:#x}（非 0 通常是权限问题）");
            }

            println!(
                "监听 VID={VENDOR_ID:#06x} PID={PRODUCT_ID:#06x}；逐个按遥控器上的键，Ctrl-C 退出。\n"
            );
            CFRunLoopRun();
        }
    }

    extern "C" fn match_callback(
        _context: *mut c_void,
        _result: IOReturn,
        _sender: *mut c_void,
        device: IOHIDDeviceRef,
    ) {
        // 生产路径在这里就 return 了（只认第一台）；探针每台都挂，好把
        // 「报文来自第二台设备」这种情况暴露出来。
        let index = DEVICE_COUNT.fetch_add(1, Ordering::SeqCst);
        println!("[设备 #{index}] 匹配成功，开始监听其 input report");

        // 回调期间缓冲区必须一直有效：每台设备泄漏一个，探针进程终身持有。
        let buf = Box::leak(Box::new([0u8; 64]));
        unsafe {
            IOHIDDeviceRegisterInputReportCallback(
                device,
                buf.as_mut_ptr(),
                64,
                report_callback,
                index as *mut c_void,
            );
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
        let index = context as usize;
        let data = unsafe { std::slice::from_raw_parts(report, report_length as usize) };
        let hex: Vec<String> = data.iter().map(|b| format!("{b:02X}")).collect();

        print!("[设备 #{index}] reportID={report_id} 原始=[{}]", hex.join(" "));

        match parse_report_usages(report_id, data) {
            None => println!("  → parse_report_usages 拒绝（reportID 或长度不符），生产路径会整份丢弃"),
            Some(usages) if usages.is_empty() => println!("  → 无按下的键（松开事件）"),
            Some(usages) => {
                let decoded: Vec<String> = usages
                    .iter()
                    .map(|&u| match RemoteButton::from_hid_usage(u) {
                        Some(b) => format!("{u:#04X}={}", b.label()),
                        None => format!("{u:#04X}=⚠️未知usage（生产路径静默忽略）"),
                    })
                    .collect();
                println!("  → {}", decoded.join(", "));
            }
        }
    }
}
