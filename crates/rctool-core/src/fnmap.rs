//! 设备级 F5 → Fn/🌐 重映射（macOS）。
//!
//! 遥控器的麦克风键在 HID 层上报键盘页 F5（usage 0x3E）。macOS 侧把**这一台
//! 设备**的 F5 重映射为 Apple 的 Fn/🌐 键之后，配合系统设置「键盘 → 听写 →
//! 快捷键：按住 🌐」，按住麦克风键 = 按住 Fn：系统听写自动开始/结束，与 ATVV
//! 音频流天然同步（同一根手指按下）。
//!
//! 机制与 `hidutil property --set UserKeyMapping` 相同：向匹配 VID/PID 的
//! IOHIDServiceClient 写 `UserKeyMapping` 属性。只影响遥控器，不碰系统全局
//! 键盘；映射随设备断开或重启消失，进程退出时主动恢复原值（[`Drop`] 兜底）。

/// 小米遥控器 HID 身份（RC003 / 2 Pro 与 ARN9 普通款同 ID）。
pub const VENDOR_ID: i64 = 0x2717;
pub const PRODUCT_ID: i64 = 0x32B8;
/// 键盘页 F5——遥控器麦克风键的 HID usage。
pub const SRC_F5: u64 = 0x0000_0007_0000_003E;
/// Apple 厂商页 top-case Fn/🌐 键。
pub const DST_FN_GLOBE: u64 = 0x0000_00FF_0000_0003;

/// 要写入的映射表：保留既有的非 F5 项，追加 F5→Fn。
fn desired_mappings(current: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> =
        current.iter().copied().filter(|(src, _)| *src != SRC_F5).collect();
    out.push((SRC_F5, DST_FN_GLOBE));
    out
}

/// 保存用的"原值"：上次进程崩溃遗留的我们自己的 F5→Fn 不算原值，
/// 否则退出恢复时会把陈旧映射原样写回去。
fn original_for_save(current: &[(u64, u64)]) -> Vec<(u64, u64)> {
    current.iter().copied().filter(|m| *m != (SRC_F5, DST_FN_GLOBE)).collect()
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{desired_mappings, original_for_save, DST_FN_GLOBE, PRODUCT_ID, SRC_F5, VENDOR_ID};
    use core_foundation::array::CFArray;
    use core_foundation::base::TCFType;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_foundation_sys::array::{CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex, CFArrayRef};
    use core_foundation_sys::base::{Boolean, CFAllocatorRef, CFGetTypeID, CFRelease, CFTypeRef};
    use core_foundation_sys::dictionary::{CFDictionaryGetTypeID, CFDictionaryGetValue, CFDictionaryRef};
    use core_foundation_sys::number::{kCFNumberSInt64Type, CFNumberGetTypeID, CFNumberGetValue, CFNumberRef};
    use core_foundation_sys::string::CFStringRef;
    use std::collections::HashMap;
    use std::ffi::c_void;

    #[repr(C)]
    pub struct OpaqueEventSystemClient(c_void);
    pub type IOHIDEventSystemClientRef = *mut OpaqueEventSystemClient;
    #[repr(C)]
    pub struct OpaqueServiceClient(c_void);
    pub type IOHIDServiceClientRef = *mut OpaqueServiceClient;

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOHIDEventSystemClientCreateSimpleClient(
            allocator: CFAllocatorRef,
        ) -> IOHIDEventSystemClientRef;
        fn IOHIDEventSystemClientCopyServices(client: IOHIDEventSystemClientRef) -> CFArrayRef;
        fn IOHIDServiceClientCopyProperty(
            service: IOHIDServiceClientRef,
            key: CFStringRef,
        ) -> CFTypeRef;
        fn IOHIDServiceClientSetProperty(
            service: IOHIDServiceClientRef,
            key: CFStringRef,
            value: CFTypeRef,
        ) -> Boolean;
        fn IOHIDServiceClientGetRegistryID(service: IOHIDServiceClientRef) -> CFTypeRef;
    }

    const KEY_MAPPING: &str = "UserKeyMapping";
    const KEY_SRC: &str = "HIDKeyboardModifierMappingSrc";
    const KEY_DST: &str = "HIDKeyboardModifierMappingDst";

    unsafe fn cfnumber_i64(value: CFTypeRef) -> Option<i64> {
        if value.is_null() || CFGetTypeID(value) != CFNumberGetTypeID() {
            return None;
        }
        let mut out: i64 = 0;
        let ok = CFNumberGetValue(
            value as CFNumberRef,
            kCFNumberSInt64Type,
            &mut out as *mut i64 as *mut c_void,
        );
        ok.then_some(out)
    }

    /// Copy 规则属性 → i64（负责释放中间对象）。
    unsafe fn copy_prop_i64(service: IOHIDServiceClientRef, key: &CFString) -> Option<i64> {
        let value = IOHIDServiceClientCopyProperty(service, key.as_concrete_TypeRef());
        if value.is_null() {
            return None;
        }
        let out = cfnumber_i64(value);
        CFRelease(value);
        out
    }

    unsafe fn read_mappings(service: IOHIDServiceClientRef, key: &CFString) -> Vec<(u64, u64)> {
        let value = IOHIDServiceClientCopyProperty(service, key.as_concrete_TypeRef());
        if value.is_null() {
            return Vec::new();
        }
        let mut out = Vec::new();
        if CFGetTypeID(value) == CFArrayGetTypeID() {
            let array = value as CFArrayRef;
            let src_key = CFString::from_static_string(KEY_SRC);
            let dst_key = CFString::from_static_string(KEY_DST);
            for i in 0..CFArrayGetCount(array) {
                let item = CFArrayGetValueAtIndex(array, i) as CFTypeRef;
                if item.is_null() || CFGetTypeID(item) != CFDictionaryGetTypeID() {
                    continue;
                }
                let dict = item as CFDictionaryRef;
                let src = CFDictionaryGetValue(dict, src_key.as_concrete_TypeRef() as *const c_void);
                let dst = CFDictionaryGetValue(dict, dst_key.as_concrete_TypeRef() as *const c_void);
                if let (Some(src), Some(dst)) =
                    (cfnumber_i64(src as CFTypeRef), cfnumber_i64(dst as CFTypeRef))
                {
                    out.push((src as u64, dst as u64));
                }
            }
        }
        CFRelease(value);
        out
    }

    fn mappings_value(mappings: &[(u64, u64)]) -> CFArray<CFDictionary<CFString, CFNumber>> {
        let dicts: Vec<CFDictionary<CFString, CFNumber>> = mappings
            .iter()
            .map(|(src, dst)| {
                CFDictionary::from_CFType_pairs(&[
                    (CFString::from_static_string(KEY_SRC), CFNumber::from(*src as i64)),
                    (CFString::from_static_string(KEY_DST), CFNumber::from(*dst as i64)),
                ])
            })
            .collect();
        CFArray::from_CFTypes(&dicts)
    }

    /// F5→Fn 重映射器。`apply` 保存每个目标服务的原映射并写入 F5→Fn；
    /// `restore`（以及 [`Drop`] 兜底）把仍在场的服务恢复原值。
    pub struct VoiceKeyMapper {
        saved: HashMap<u64, Vec<(u64, u64)>>,
    }

    impl Default for VoiceKeyMapper {
        fn default() -> Self {
            Self::new()
        }
    }

    impl VoiceKeyMapper {
        pub fn new() -> Self {
            Self { saved: HashMap::new() }
        }

        /// 对所有匹配 VID/PID 的 HID 服务应用 F5→Fn，返回写入并读回校验
        /// 成功的服务数。设备不在场时返回 0（重连循环会自然重试）。
        pub fn apply(&mut self) -> anyhow::Result<usize> {
            unsafe {
                let client = IOHIDEventSystemClientCreateSimpleClient(std::ptr::null_mut());
                anyhow::ensure!(!client.is_null(), "无法创建 IOHID 事件系统客户端");
                let result = self.apply_inner(client);
                CFRelease(client as CFTypeRef);
                result
            }
        }

        unsafe fn apply_inner(&mut self, client: IOHIDEventSystemClientRef) -> anyhow::Result<usize> {
            let services = IOHIDEventSystemClientCopyServices(client);
            anyhow::ensure!(!services.is_null(), "无法枚举 IOHID 服务");
            let vid_key = CFString::from_static_string("VendorID");
            let pid_key = CFString::from_static_string("ProductID");
            let map_key = CFString::from_static_string(KEY_MAPPING);
            let mut applied = 0usize;
            for i in 0..CFArrayGetCount(services) {
                let service = CFArrayGetValueAtIndex(services, i) as IOHIDServiceClientRef;
                if service.is_null()
                    || copy_prop_i64(service, &vid_key) != Some(VENDOR_ID)
                    || copy_prop_i64(service, &pid_key) != Some(PRODUCT_ID)
                {
                    continue;
                }
                // Get 规则：registry ID 不需要释放。
                let Some(rid) = cfnumber_i64(IOHIDServiceClientGetRegistryID(service)) else {
                    continue;
                };
                let rid = rid as u64;
                let current = read_mappings(service, &map_key);
                self.saved.entry(rid).or_insert_with(|| original_for_save(&current));
                let value = mappings_value(&desired_mappings(&current));
                if IOHIDServiceClientSetProperty(
                    service,
                    map_key.as_concrete_TypeRef(),
                    value.as_CFTypeRef(),
                ) != 0
                {
                    if read_mappings(service, &map_key).contains(&(SRC_F5, DST_FN_GLOBE)) {
                        applied += 1;
                    } else {
                        log::warn!("F5→Fn 写入后读回校验失败（HID 服务 {rid:#x}）");
                    }
                } else {
                    log::warn!("F5→Fn 写入被拒绝（HID 服务 {rid:#x}）");
                }
            }
            CFRelease(services as CFTypeRef);
            Ok(applied)
        }

        /// 恢复仍在场服务的原映射，返回恢复数。服务已随设备消失的条目直接
        /// 丢弃（其映射也随设备一起消失了）；写失败的条目保留待下次重试。
        pub fn restore(&mut self) -> usize {
            if self.saved.is_empty() {
                return 0;
            }
            unsafe {
                let client = IOHIDEventSystemClientCreateSimpleClient(std::ptr::null_mut());
                if client.is_null() {
                    return 0;
                }
                let restored = self.restore_inner(client);
                CFRelease(client as CFTypeRef);
                restored
            }
        }

        unsafe fn restore_inner(&mut self, client: IOHIDEventSystemClientRef) -> usize {
            let services = IOHIDEventSystemClientCopyServices(client);
            if services.is_null() {
                return 0;
            }
            let vid_key = CFString::from_static_string("VendorID");
            let pid_key = CFString::from_static_string("ProductID");
            let map_key = CFString::from_static_string(KEY_MAPPING);
            let mut restored = 0usize;
            let mut present: Vec<u64> = Vec::new();
            for i in 0..CFArrayGetCount(services) {
                let service = CFArrayGetValueAtIndex(services, i) as IOHIDServiceClientRef;
                if service.is_null()
                    || copy_prop_i64(service, &vid_key) != Some(VENDOR_ID)
                    || copy_prop_i64(service, &pid_key) != Some(PRODUCT_ID)
                {
                    continue;
                }
                let Some(rid) = cfnumber_i64(IOHIDServiceClientGetRegistryID(service)) else {
                    continue;
                };
                let rid = rid as u64;
                present.push(rid);
                let Some(original) = self.saved.get(&rid) else { continue };
                let value = mappings_value(original);
                if IOHIDServiceClientSetProperty(
                    service,
                    map_key.as_concrete_TypeRef(),
                    value.as_CFTypeRef(),
                ) != 0
                {
                    self.saved.remove(&rid);
                    restored += 1;
                } else {
                    log::warn!("恢复 F5 原映射失败（HID 服务 {rid:#x}），保留待重试");
                }
            }
            CFRelease(services as CFTypeRef);
            // 不在场的服务：映射已随设备消失，条目作废。
            self.saved.retain(|rid, _| present.contains(rid));
            restored
        }
    }

    impl Drop for VoiceKeyMapper {
        fn drop(&mut self) {
            let restored = self.restore();
            if restored > 0 {
                log::info!("已恢复 F5 原映射（{restored} 个 HID 服务）");
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    /// 非 macOS 平台的占位实现：Windows/Linux 的听写触发方案不同
    /// （Windows 走合成热键如 Win+H，Linux 待定），不在本模块范围。
    pub struct VoiceKeyMapper;

    impl Default for VoiceKeyMapper {
        fn default() -> Self {
            Self::new()
        }
    }

    impl VoiceKeyMapper {
        pub fn new() -> Self {
            Self
        }

        pub fn apply(&mut self) -> anyhow::Result<usize> {
            Ok(0)
        }

        pub fn restore(&mut self) -> usize {
            0
        }
    }
}

pub use imp::VoiceKeyMapper;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desired_appends_f5_and_keeps_others() {
        assert_eq!(desired_mappings(&[]), vec![(SRC_F5, DST_FN_GLOBE)]);
        let existing = vec![(0xAA, 0xBB), (SRC_F5, 0x1234)];
        assert_eq!(
            desired_mappings(&existing),
            vec![(0xAA, 0xBB), (SRC_F5, DST_FN_GLOBE)]
        );
    }

    #[test]
    fn original_excludes_our_stale_pair_but_keeps_foreign_f5() {
        // 崩溃遗留的我们自己的 F5→Fn：不算原值。
        assert_eq!(original_for_save(&[(SRC_F5, DST_FN_GLOBE)]), vec![]);
        // 别人配置的 F5 映射：算原值，退出时要还回去。
        let foreign = vec![(SRC_F5, 0x1234), (0xAA, 0xBB)];
        assert_eq!(original_for_save(&foreign), foreign);
    }
}
