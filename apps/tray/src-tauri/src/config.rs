//! 持久化配置。

use rctool_core::keymap::{Action, AppKeyMaps, KeyMap, RemoteButton};
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
    /// 按应用的覆盖层（macOS）。数组而非映射：顺序即界面列表顺序。
    pub app_profiles: Vec<AppProfile>,
    /// macOS：关闭主窗口时把图标从 Dock 移出（切到 Accessory 激活策略），
    /// 只留菜单栏图标。再次显示窗口时切回 Regular。
    pub hide_dock_on_close: bool,
    /// 绑定的遥控器。绑定后语音桥接与在场检测都只认这一台；None = 沿用
    /// 「第一台匹配的就用」。
    pub bound_device: Option<BoundDevice>,
    /// macOS：遥控器在场期间阻止系统 idle sleep。
    pub keep_awake: bool,
    /// macOS：遥控器在场且屏幕锁着时，自动把钥匙串里的密码敲进登录窗。
    /// 高风险，默认关闭——见 `rctool_core::screen` 顶部的说明。
    pub auto_unlock: bool,
}

/// 绑定的遥控器。`id` 是匹配键（`bluest::DeviceId` 的字符串形式），
/// `name` 只用于界面显示。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BoundDevice {
    pub id: String,
    pub name: String,
}

/// 一个应用的按键覆盖层。只存与全局不同的键——见
/// [`rctool_core::keymap::AppProfile`] 的差量设计说明。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppProfile {
    pub bundle_id: String,
    /// 添加时记下的显示名；应用改名不影响匹配（匹配只看 bundle_id）。
    pub name: String,
    pub enabled: bool,
    pub bindings: HashMap<String, String>,
}

impl Default for AppProfile {
    fn default() -> Self {
        Self {
            bundle_id: String::new(),
            name: String::new(),
            enabled: true,
            bindings: HashMap::new(),
        }
    }
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
            app_profiles: Vec::new(),
            hide_dock_on_close: true,
            bound_device: None,
            keep_awake: false,
            auto_unlock: false,
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

    /// 全局映射 + 各应用覆盖层。运行时按前台应用 `resolve` 出实际映射。
    pub fn app_key_maps(&self) -> AppKeyMaps {
        let mut maps = AppKeyMaps::new(self.key_map());
        for stored in &self.app_profiles {
            let profile = maps.profile_mut(&stored.bundle_id, &stored.name);
            profile.enabled = stored.enabled;
            for (button_id, action_id) in &stored.bindings {
                if let (Some(button), Some(action)) =
                    (RemoteButton::from_id(button_id), Action::from_id(action_id))
                {
                    profile.set(button, action);
                }
            }
        }
        maps
    }

    /// 取某应用的覆盖层，不存在则新建（首次改键即建层）。
    pub fn app_profile_mut(&mut self, bundle_id: &str, name: &str) -> &mut AppProfile {
        match self.app_profiles.iter().position(|p| p.bundle_id == bundle_id) {
            Some(i) => &mut self.app_profiles[i],
            None => {
                self.app_profiles.push(AppProfile {
                    bundle_id: bundle_id.to_string(),
                    name: name.to_string(),
                    ..AppProfile::default()
                });
                self.app_profiles.last_mut().expect("刚推入")
            }
        }
    }

    pub fn remove_app_profile(&mut self, bundle_id: &str) {
        self.app_profiles.retain(|p| p.bundle_id != bundle_id);
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
