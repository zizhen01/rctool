//! Windows：语音流开始/结束时合成 Win+H，触发系统语音输入面板。
//!
//! 对应 macOS 的 F5→Fn 设备级重映射（[`rctool_core::fnmap`]）。Windows 没有
//! 设备级改键机制，Win+H 是切换语义：流开始时点一下打开语音输入，流结束时
//! 再点一下关闭，不跨流按住任何键。Linux 无系统级听写，桥接只提供虚拟麦克风。

#[cfg(windows)]
pub fn on_stream(active: bool) {
    // 开始与结束各触发一次切换。
    let _ = active;
    tap_win_h();
}

#[cfg(windows)]
fn tap_win_h() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_LWIN,
    };

    const VK_H: VIRTUAL_KEY = VIRTUAL_KEY(0x48);

    fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    let sequence = [
        key(VK_LWIN, false),
        key(VK_H, false),
        key(VK_H, true),
        key(VK_LWIN, true),
    ];
    let sent = unsafe { SendInput(&sequence, std::mem::size_of::<INPUT>() as i32) };
    if sent != sequence.len() as u32 {
        log::warn!("Win+H 合成不完整（{sent}/{}）", sequence.len());
    }
}

#[cfg(not(windows))]
pub fn on_stream(_active: bool) {}
