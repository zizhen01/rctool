//! 持久化配置。

use rctool_core::keymap::{Action, KeyMap, RemoteButton};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 语音输出设备名（回环设备）。None = 未选。
    pub output_device: Option<String>,
    /// 数字增益 dB。
    pub gain_db: f64,
    /// macOS：F5→Fn 听写触发重映射。
    pub fn_remap: bool,
    /// Windows：语音流开始/结束时合成 Win+H 触发系统语音输入。
    pub win_hotkey: bool,
    /// 是否启用按键映射（macOS；需要输入监控/辅助功能权限）。
    pub key_mapping: bool,
    /// 按键覆盖：button_id → action_id。缺省用出厂默认。
    pub bindings: HashMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            output_device: None,
            gain_db: 0.0,
            fn_remap: true,
            win_hotkey: true,
            key_mapping: false,
            bindings: HashMap::new(),
        }
    }
}

impl Config {
    pub fn load(path: &PathBuf) -> Config {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                log::warn!("配置解析失败，使用默认值: {e}");
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self, path: &PathBuf) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_vec_pretty(self) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(path, bytes) {
                    log::warn!("配置保存失败: {e}");
                }
            }
            Err(e) => log::warn!("配置序列化失败: {e}"),
        }
    }

    /// 由存储的覆盖构造完整按键映射（以出厂默认为底）。
    pub fn key_map(&self) -> KeyMap {
        let mut map = KeyMap::with_defaults();
        for (button_id, action_id) in &self.bindings {
            if let (Some(button), Some(action)) =
                (RemoteButton::from_id(button_id), Action::from_id(action_id))
            {
                map.set(button, action);
            }
        }
        map
    }
}
