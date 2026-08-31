//! RCTool 托盘应用后端。
//!
//! 职责：管理配置、桥接（BLE→回环）生命周期、按键映射（macOS），并把状态
//! 推给设置界面与托盘。核心逻辑全部来自 `rctool-core`，这里只做编排与 UI。

#[cfg(target_os = "macos")]
mod autostart;
mod config;
mod dictation;
#[cfg(target_os = "macos")]
mod keychain;
mod watch;

use config::{BoundDevice, Config};
use rctool_core::bridge::{self, BridgeOptions, BridgeStatus, StatusCallback};
use rctool_core::keymap::{Action, Disposition, RemoteButton};
use rctool_core::loopback::{self, LoopbackSink};
use rctool_core::sink::MultiSink;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::menu::{MenuBuilder, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tokio_util::sync::CancellationToken;

#[cfg(target_os = "macos")]
use rctool_core::frontapp::{self, FrontApp};
#[cfg(target_os = "macos")]
use rctool_core::hidmap::{HidMapper, Permissions};

struct BridgeHandle {
    token: CancellationToken,
    join: tauri::async_runtime::JoinHandle<()>,
}

/// 前台应用状态（macOS）。
///
/// 锁纪律：本锁与 `config` 锁**不同时持有**——取需要的值就立刻释放，避免
/// 前台切换回调（主线程）与设置界面命令（IPC 线程）互相等待。
#[cfg(target_os = "macos")]
#[derive(Default)]
struct FrontState {
    /// 真实前台应用，含 RCTool 自己。映射解析用这个。
    active: Option<FrontApp>,
    /// 最近一个不是 RCTool 自己的前台应用。设置界面正被看着的时候，前台就是
    /// RCTool，所以"添加当前应用"要用的是这个。
    recent: Option<FrontApp>,
}

struct AppState {
    config_path: PathBuf,
    config: Mutex<Config>,
    bridge: Mutex<Option<BridgeHandle>>,
    /// 在场监管任务。与桥接彼此独立——没配语音输出也照常跑。
    presence: Mutex<Option<watch::Handle>>,
    #[cfg(target_os = "macos")]
    hid: Mutex<Option<HidMapper>>,
    #[cfg(target_os = "macos")]
    front: Mutex<FrontState>,
    app: AppHandle,
}

impl AppState {
    fn save_config(&self) {
        self.config.lock().unwrap().save(&self.config_path);
    }
}

// ---------------------------------------------------------------------------
// DTO
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ConfigDto {
    output_device: Option<String>,
    gain_db: f64,
    fn_remap: bool,
    win_hotkey: bool,
    key_mapping: bool,
    hide_dock_on_close: bool,
    running: bool,
    /// "macos" / "windows" / "linux"，前端按平台裁剪界面。
    platform: &'static str,
    bound_device: Option<BoundDeviceDto>,
    keep_awake: bool,
    auto_unlock: bool,
    /// 钥匙串里是否已存密码。**只报有无，永不回传明文。**
    has_unlock_password: bool,
    launch_at_login: bool,
}

#[derive(Clone, Serialize)]
struct BoundDeviceDto {
    id: String,
    name: String,
}

/// 可绑定的候选遥控器。
#[derive(Serialize)]
struct RemoteDto {
    id: String,
    name: String,
    connected: bool,
    /// 是否就是当前已绑定的那台。
    bound: bool,
}

#[derive(Serialize)]
struct OutputDto {
    name: String,
    is_default: bool,
    is_loopback: bool,
}

#[derive(Serialize)]
struct ActionDto {
    id: String,
    label: String,
}

#[derive(Serialize)]
struct ButtonDto {
    id: String,
    label: String,
    action_id: String,
    /// 该键是否真正接管了行为（拦截/注入），直通键为 false。
    managed: bool,
}

/// 一个可被指定覆盖层的应用（运行中的应用 / 最近前台应用）。
#[derive(Clone, Serialize)]
struct AppDto {
    bundle_id: String,
    name: String,
    /// 是否已有覆盖层。
    has_profile: bool,
}

/// 某应用相对全局映射的**一条**差异。界面只渲染差异，不重复整张映射表。
#[derive(Serialize)]
struct DiffDto {
    button_id: String,
    button_label: String,
    base_action_id: String,
    base_action_label: String,
    action_id: String,
    action_label: String,
}

#[derive(Serialize)]
struct AppProfileDto {
    bundle_id: String,
    name: String,
    enabled: bool,
    /// 此刻是否正是前台应用（界面上标"生效中"）。
    active: bool,
    diffs: Vec<DiffDto>,
}

#[derive(Serialize)]
struct PermissionsDto {
    input_monitoring: bool,
    accessibility: bool,
    /// 平台是否需要这些权限（非 macOS 为 false）。
    applicable: bool,
}

#[derive(Clone, Serialize)]
struct StatusDto {
    kind: String,
    detail: String,
    streaming: bool,
}

impl From<BridgeStatus> for StatusDto {
    fn from(s: BridgeStatus) -> Self {
        match s {
            BridgeStatus::Searching => {
                StatusDto { kind: "searching".into(), detail: "正在查找遥控器…".into(), streaming: false }
            }
            BridgeStatus::Connected(name) => {
                StatusDto { kind: "connected".into(), detail: format!("已连接 {name}"), streaming: false }
            }
            BridgeStatus::Streaming(on) => StatusDto {
                kind: "connected".into(),
                detail: if on { "语音输入中…".into() } else { "已连接，待命".into() },
                streaming: on,
            },
            BridgeStatus::Disconnected => {
                StatusDto { kind: "disconnected".into(), detail: "已断开，重连中…".into(), streaming: false }
            }
            BridgeStatus::Error(e) => StatusDto { kind: "error".into(), detail: e, streaming: false },
        }
    }
}

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

#[tauri::command]
fn get_config(state: State<AppState>) -> ConfigDto {
    let c = state.config.lock().unwrap();
    ConfigDto {
        output_device: c.output_device.clone(),
        gain_db: c.gain_db,
        fn_remap: c.fn_remap,
        win_hotkey: c.win_hotkey,
        key_mapping: c.key_mapping,
        hide_dock_on_close: c.hide_dock_on_close,
        running: state.bridge.lock().unwrap().is_some(),
        platform: std::env::consts::OS,
        bound_device: c
            .bound_device
            .as_ref()
            .map(|d| BoundDeviceDto { id: d.id.clone(), name: d.name.clone() }),
        keep_awake: c.keep_awake,
        auto_unlock: c.auto_unlock,
        has_unlock_password: unlock_password_stored(),
        launch_at_login: launch_at_login_enabled(),
    }
}

/// 是否已设置开机自启。非 macOS 恒为 false（未实现）。
fn launch_at_login_enabled() -> bool {
    #[cfg(target_os = "macos")]
    {
        autostart::is_enabled()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// 钥匙串里是否已存解锁密码。非 macOS 恒为 false。
fn unlock_password_stored() -> bool {
    #[cfg(target_os = "macos")]
    {
        keychain::has_password()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[tauri::command]
fn list_outputs() -> Vec<OutputDto> {
    let default = loopback::default_output_name();
    loopback::output_device_names()
        .into_iter()
        .map(|name| OutputDto {
            is_default: Some(&name) == default.as_ref(),
            is_loopback: loopback::is_known_loopback(&name),
            name,
        })
        .collect()
}

#[tauri::command]
fn get_actions() -> Vec<ActionDto> {
    let mut out = vec![
        ActionDto { id: Action::Native.id().into(), label: Action::Native.label().into() },
        ActionDto { id: Action::Disabled.id().into(), label: Action::Disabled.label().into() },
    ];
    out.extend(Action::ASSIGNABLE.into_iter().map(|a| ActionDto {
        id: a.id().into(),
        label: a.label().into(),
    }));
    out
}

#[tauri::command]
fn get_buttons(state: State<AppState>) -> Vec<ButtonDto> {
    let map = state.config.lock().unwrap().key_map();
    RemoteButton::ALL
        .into_iter()
        .map(|b| ButtonDto {
            id: b.id().into(),
            label: b.label().into(),
            action_id: map.action(b).id().into(),
            managed: !matches!(map.disposition(b), Disposition::Passthrough),
        })
        .collect()
}

#[tauri::command]
fn set_binding(state: State<AppState>, button_id: String, action_id: String) -> Result<(), String> {
    let (button, action) = RemoteButton::from_id(&button_id)
        .zip(Action::from_id(&action_id))
        .ok_or_else(|| "未知按键或动作".to_string())?;
    {
        let mut c = state.config.lock().unwrap();
        c.bindings.insert(button.id().into(), action.id().into());
    }
    state.save_config();
    refresh_hid(&state);
    Ok(())
}

#[tauri::command]
fn reset_bindings(state: State<AppState>) {
    state.config.lock().unwrap().bindings.clear();
    state.save_config();
    refresh_hid(&state);
}

// --- 按应用映射 ---
//
// 模型见 rctool_core::keymap::AppProfile：覆盖层只存"与全局不同的键"。因此这
// 组命令给前端的也只有差异，界面不需要复制一遍主界面的整张映射表。

/// 当前正在运行、可指定覆盖层的应用（不含 RCTool 自己）。
#[tauri::command]
fn list_running_apps(state: State<AppState>) -> Vec<AppDto> {
    #[cfg(target_os = "macos")]
    {
        let known = profile_bundle_ids(&state);
        let own = state.app.config().identifier.clone();
        frontapp::running_apps()
            .into_iter()
            .filter(|a| a.bundle_id != own)
            .map(|a| AppDto {
                has_profile: known.contains(&a.bundle_id),
                bundle_id: a.bundle_id,
                name: a.name,
            })
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = state;
        Vec::new()
    }
}

/// 最近一个不是 RCTool 的前台应用——"添加当前应用"用它。
#[tauri::command]
fn get_front_app(state: State<AppState>) -> Option<AppDto> {
    #[cfg(target_os = "macos")]
    {
        let recent = state.front.lock().unwrap().recent.clone()?;
        Some(app_dto(&state, recent))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = state;
        None
    }
}

#[tauri::command]
fn get_app_profiles(state: State<AppState>) -> Vec<AppProfileDto> {
    let active = active_bundle_id(&state);
    let c = state.config.lock().unwrap();
    let maps = c.app_key_maps();
    c.app_profiles
        .iter()
        .map(|p| AppProfileDto {
            active: active.as_deref() == Some(p.bundle_id.as_str()),
            bundle_id: p.bundle_id.clone(),
            name: p.name.clone(),
            enabled: p.enabled,
            diffs: maps
                .diff(&p.bundle_id)
                .into_iter()
                .map(|d| DiffDto {
                    button_id: d.button.id().into(),
                    button_label: d.button.label().into(),
                    base_action_id: d.base.id().into(),
                    base_action_label: d.base.label().into(),
                    action_id: d.app.id().into(),
                    action_label: d.app.label().into(),
                })
                .collect(),
        })
        .collect()
}

#[tauri::command]
fn add_app_profile(state: State<AppState>, bundle_id: String, name: String) -> Result<(), String> {
    if bundle_id.trim().is_empty() {
        return Err("bundle id 不能为空".into());
    }
    state.config.lock().unwrap().app_profile_mut(&bundle_id, &name);
    state.save_config();
    Ok(())
}

#[tauri::command]
fn remove_app_profile(state: State<AppState>, bundle_id: String) {
    state.config.lock().unwrap().remove_app_profile(&bundle_id);
    state.save_config();
    refresh_hid(&state);
}

#[tauri::command]
fn set_app_profile_enabled(state: State<AppState>, bundle_id: String, enabled: bool) {
    {
        let mut c = state.config.lock().unwrap();
        if let Some(p) = c.app_profiles.iter_mut().find(|p| p.bundle_id == bundle_id) {
            p.enabled = enabled;
        }
    }
    state.save_config();
    refresh_hid(&state);
}

/// 设置（或清除）某应用下某键的覆盖。`action_id` 为 None 即清除，回落全局。
///
/// 选成与全局**相同**的动作也按清除处理：留一条对运行时毫无影响的覆盖，只会
/// 让"差异列表"里出现一行看不出差在哪的记录。
#[tauri::command]
fn set_app_binding(
    state: State<AppState>,
    bundle_id: String,
    name: Option<String>,
    button_id: String,
    action_id: Option<String>,
) -> Result<(), String> {
    let button = RemoteButton::from_id(&button_id).ok_or_else(|| "未知按键".to_string())?;
    let action = match action_id.as_deref() {
        None => None,
        Some(id) => Some(Action::from_id(id).ok_or_else(|| "未知动作".to_string())?),
    };
    {
        let mut c = state.config.lock().unwrap();
        let base = c.key_map().action(button);
        let display = name.unwrap_or_else(|| bundle_id.clone());
        let profile = c.app_profile_mut(&bundle_id, &display);
        match action {
            Some(action) if action != base => {
                profile.bindings.insert(button.id().into(), action.id().into());
            }
            _ => {
                profile.bindings.remove(button.id());
            }
        }
    }
    state.save_config();
    refresh_hid(&state);
    Ok(())
}

/// 清空某应用的全部覆盖（保留空的覆盖层本身）。
#[tauri::command]
fn clear_app_bindings(state: State<AppState>, bundle_id: String) {
    {
        let mut c = state.config.lock().unwrap();
        if let Some(p) = c.app_profiles.iter_mut().find(|p| p.bundle_id == bundle_id) {
            p.bindings.clear();
        }
    }
    state.save_config();
    refresh_hid(&state);
}

/// 当前前台应用的 bundle id（映射解析用）。非 macOS 恒为 None。
///
/// 缓存为空时现问一次：应用刚启动、NSApplication 还没跑起来的那一小段里
/// `frontmostApplication` 可能是 nil，此时监听拿不到首个值，别让它一直空着。
fn active_bundle_id(state: &AppState) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let cached = state.front.lock().unwrap().active.as_ref().map(|a| a.bundle_id.clone());
        cached.or_else(|| frontapp::frontmost().map(|a| a.bundle_id))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = state;
        None
    }
}

fn profile_bundle_ids(state: &AppState) -> Vec<String> {
    state
        .config
        .lock()
        .unwrap()
        .app_profiles
        .iter()
        .map(|p| p.bundle_id.clone())
        .collect()
}

#[cfg(target_os = "macos")]
fn app_dto(state: &AppState, app: FrontApp) -> AppDto {
    let known = profile_bundle_ids(state);
    AppDto {
        has_profile: known.contains(&app.bundle_id),
        bundle_id: app.bundle_id,
        name: app.name,
    }
}

#[tauri::command]
fn set_output(state: State<AppState>, name: Option<String>) {
    state.config.lock().unwrap().output_device = name;
    state.save_config();
}

#[tauri::command]
fn set_gain(state: State<AppState>, gain_db: f64) {
    state.config.lock().unwrap().gain_db = gain_db.clamp(-24.0, 24.0);
    state.save_config();
}

#[tauri::command]
fn set_fn_remap(state: State<AppState>, enabled: bool) {
    state.config.lock().unwrap().fn_remap = enabled;
    state.save_config();
}

#[tauri::command]
fn set_win_hotkey(state: State<AppState>, enabled: bool) {
    state.config.lock().unwrap().win_hotkey = enabled;
    state.save_config();
}

#[tauri::command]
fn set_key_mapping(state: State<AppState>, enabled: bool) {
    state.config.lock().unwrap().key_mapping = enabled;
    state.save_config();
    refresh_hid(&state);
}

#[tauri::command]
fn set_hide_dock_on_close(state: State<AppState>, enabled: bool) {
    state.config.lock().unwrap().hide_dock_on_close = enabled;
    state.save_config();
    // 关掉的瞬间要把图标找回来——否则得先开一次窗口才恢复。
    if !enabled {
        set_dock_visible(&state.app, true);
    }
}

// ---------------------------------------------------------------------------
// 遥控器绑定 / 防睡眠 / 自动解锁
// ---------------------------------------------------------------------------

/// 扫描可绑定的遥控器。会跑一轮广播扫描，故耗时到秒级。
#[tauri::command]
async fn list_remotes(state: State<'_, AppState>) -> Result<Vec<RemoteDto>, String> {
    let bound = state.config.lock().unwrap().bound_device.as_ref().map(|d| d.id.clone());
    let found = rctool_core::presence::list_remotes(std::time::Duration::from_secs(6))
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(found
        .into_iter()
        .map(|r| RemoteDto {
            bound: bound.as_deref() == Some(r.id.as_str()),
            id: r.id,
            name: r.name,
            connected: r.connected,
        })
        .collect())
}

/// 绑定到指定遥控器。桥接与在场检测此后都只认这一台。
#[tauri::command]
fn bind_device(state: State<AppState>, id: String, name: String) {
    state.config.lock().unwrap().bound_device = Some(BoundDevice { id, name });
    state.save_config();
    restart_presence(&state);
    restart_bridge_if_running(&state);
}

#[tauri::command]
fn unbind_device(state: State<AppState>) {
    state.config.lock().unwrap().bound_device = None;
    state.save_config();
    restart_presence(&state);
    restart_bridge_if_running(&state);
}

#[tauri::command]
fn set_keep_awake(state: State<AppState>, enabled: bool) {
    state.config.lock().unwrap().keep_awake = enabled;
    state.save_config();
    restart_presence(&state);
}

/// 开启自动解锁。没存密码就直接拒绝——与其让它静默不生效，不如在这里说清楚。
#[tauri::command]
fn set_auto_unlock(state: State<AppState>, enabled: bool) -> Result<(), String> {
    if enabled && !unlock_password_stored() {
        return Err("请先设置解锁密码".into());
    }
    state.config.lock().unwrap().auto_unlock = enabled;
    state.save_config();
    restart_presence(&state);
    Ok(())
}

/// 把登录密码存进钥匙串。密码不落配置、不落日志，存完就只剩钥匙串里那一份。
#[tauri::command]
fn set_unlock_password(_password: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if _password.is_empty() {
            return Err("密码不能为空".into());
        }
        keychain::set_password(&_password)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("自动解锁目前仅支持 macOS".into())
    }
}

/// 清除钥匙串里的密码，并连带关掉自动解锁——没有密码的自动解锁只是个空壳开关。
#[tauri::command]
fn clear_unlock_password(state: State<AppState>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    keychain::clear_password()?;
    state.config.lock().unwrap().auto_unlock = false;
    state.save_config();
    restart_presence(&state);
    Ok(())
}

/// 开关开机自启。状态存在 LaunchAgent plist 里，不进配置文件。
#[tauri::command]
fn set_launch_at_login(_enabled: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        autostart::set_enabled(_enabled)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("开机自启目前仅支持 macOS".into())
    }
}

#[tauri::command]
fn get_permissions() -> PermissionsDto {
    #[cfg(target_os = "macos")]
    {
        let p = Permissions::query();
        PermissionsDto {
            input_monitoring: p.input_monitoring,
            accessibility: p.accessibility,
            applicable: true,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        PermissionsDto { input_monitoring: true, accessibility: true, applicable: false }
    }
}

/// 打开「隐私与安全性」下的指定面板。anchor 为 TCC 服务名，如
/// `Privacy_ListenEvent`（输入监控）、`Privacy_Accessibility`（辅助功能）。
#[cfg(target_os = "macos")]
fn open_privacy_pane(anchor: &str) {
    let _ = std::process::Command::new("open")
        .arg(format!(
            "x-apple.systempreferences:com.apple.preference.security?{anchor}"
        ))
        .spawn();
}

/// 输入监控：读取遥控器 HID 报文所需。
#[tauri::command]
fn request_input_monitoring() {
    #[cfg(target_os = "macos")]
    {
        // 首次请求能弹出系统对话框；弹不出（已问过/已拒绝）就打开面板。
        if !Permissions::request_input_monitoring() {
            open_privacy_pane("Privacy_ListenEvent");
        }
    }
}

/// 辅助功能：注入按键与创建拦截 tap 所需。
#[tauri::command]
fn request_accessibility() {
    #[cfg(target_os = "macos")]
    {
        // 先登记（否则设置列表里根本没有本 app），再打开面板让用户开开关。
        if !Permissions::request_accessibility() {
            open_privacy_pane("Privacy_Accessibility");
        }
    }
    // Windows/Linux：桥接与听写触发不需要额外系统权限。
}

/// 回环设备缺失时的安装引导，按平台与发行版本分级：
/// - macOS full：内置 BlackHole 官方安装器（GPL-3.0 与本项目同证，可合法
///   内嵌），用系统 Installer 图形向导打开，提权由系统处理；
/// - macOS lite：有 Homebrew 则终端执行 brew install blackhole-2ch（cask 装
///   pkg 需管理员密码，必须 TTY 交互），否则打开官网下载页；
/// - Windows：VB-Cable 许可禁止捆绑文件，改为运行时从**官方源**下载并启动
///   其安装器（等效 full 体验），失败回退打开官网；
/// - Linux：直接创建 null-sink，无需任何下载。
#[tauri::command]
fn setup_loopback(app: AppHandle) -> Result<String, String> {
    let _ = &app;
    #[cfg(target_os = "macos")]
    {
        if let Some(pkg) = bundled_blackhole_pkg(&app) {
            std::process::Command::new("open")
                .arg(&pkg)
                .spawn()
                .map_err(|e| format!("无法打开内置安装器: {e}"))?;
            return Ok("已打开内置 BlackHole 安装器，按向导完成后点「重新检测」".into());
        }
        if let Some(brew) = find_brew() {
            launch_brew_install_in_terminal(&brew)?;
            Ok("已在终端中开始安装 BlackHole（需要输入管理员密码）；完成后点「重新检测」".into())
        } else {
            open_url("https://existential.audio/blackhole/")?;
            Ok("未检测到 Homebrew，已打开 BlackHole 下载页；安装后点「重新检测」".into())
        }
    }
    #[cfg(target_os = "windows")]
    {
        match download_and_launch_vbcable() {
            Ok(message) => Ok(message),
            Err(e) => {
                log::warn!("VB-Cable 自动获取失败，回退官网: {e}");
                open_url("https://vb-audio.com/Cable/")?;
                Ok("自动获取失败，已打开 VB-Cable 下载页；安装后点「重新检测」".into())
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        let status = std::process::Command::new("pactl")
            .args([
                "load-module",
                "module-null-sink",
                "sink_name=RCTool",
                "sink_properties=device.description=RCTool",
            ])
            .status()
            .map_err(|e| format!("无法执行 pactl: {e}"))?;
        if status.success() {
            Ok("已创建 RCTool 虚拟设备，点「重新检测」后选择它".into())
        } else {
            Err("pactl 执行失败（需要 PulseAudio / PipeWire）".into())
        }
    }
}

/// full 版内置的 BlackHole 安装器（resources/BlackHole*.pkg）；lite 版此目录
/// 为空。按前缀扫描以免固定死版本号文件名。
#[cfg(target_os = "macos")]
fn bundled_blackhole_pkg(app: &AppHandle) -> Option<std::path::PathBuf> {
    let dir = app
        .path()
        .resolve("resources", tauri::path::BaseDirectory::Resource)
        .ok()?;
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .find(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("BlackHole") && n.ends_with(".pkg"))
                .unwrap_or(false)
        })
}

/// Windows：从 VB-Audio 官方源下载驱动包并启动安装器。不再分发文件——
/// 下载发生在用户设备、来自官方 URL，等效于用户手动操作。
#[cfg(target_os = "windows")]
fn download_and_launch_vbcable() -> Result<String, String> {
    // 官方直链版本号随官网更新；过期会 404，由调用方回退到打开网页。
    const URL: &str = "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip";
    let dir = std::env::temp_dir().join("rctool-vbcable");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建临时目录失败: {e}"))?;
    let zip = dir.join("VBCABLE_Driver_Pack.zip");
    run_powershell(&format!("Invoke-WebRequest -Uri '{URL}' -OutFile '{}'", zip.display()))?;
    run_powershell(&format!("Expand-Archive -Force '{}' '{}'", zip.display(), dir.display()))?;
    let setup = dir.join("VBCABLE_Setup_x64.exe");
    if !setup.exists() {
        return Err("下载的安装包结构不符合预期".into());
    }
    // 驱动安装必须提权：RunAs 触发 UAC 确认。
    run_powershell(&format!("Start-Process -FilePath '{}' -Verb RunAs", setup.display()))?;
    Ok("已从官方源获取并启动 VB-Cable 安装器（需管理员确认）；装完点「重新检测」".into())
}

#[cfg(target_os = "windows")]
fn run_powershell(command: &str) -> Result<(), String> {
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", command])
        .status()
        .map_err(|e| format!("无法执行 PowerShell: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("PowerShell 命令失败（{status}）"))
    }
}

#[cfg(target_os = "macos")]
fn find_brew() -> Option<String> {
    ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(|s| s.to_string())
}

/// 生成一次性 .command 脚本并用 Terminal 打开执行。`open` 一个 .command
/// 文件会让 Terminal 运行它——用户在终端里看到进度并输入 sudo 密码，
/// 且无需向本应用授予 AppleScript 自动化权限。
#[cfg(target_os = "macos")]
fn launch_brew_install_in_terminal(brew: &str) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    // 语音是 16 kHz 单声道，2ch 版本足够（16/64ch 面向 DAW 多轨路由）。
    let script = format!(
        "#!/bin/zsh\nset -e\necho 'RCTool: 通过 Homebrew 安装 BlackHole (2ch)…'\n\
         \"{brew}\" install blackhole-2ch\necho\n\
         echo '安装完成。回到 RCTool 点「重新检测」即可选择 BlackHole 2ch。'\n"
    );
    let path = std::env::temp_dir().join("rctool-install-blackhole.command");
    std::fs::write(&path, script).map_err(|e| format!("写入安装脚本失败: {e}"))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("设置脚本权限失败: {e}"))?;
    std::process::Command::new("open")
        .arg(&path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("无法打开终端执行安装: {e}"))
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn open_url(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    result.map(|_| ()).map_err(|e| format!("无法打开浏览器: {e}"))
}

#[tauri::command]
async fn start_bridge(state: State<'_, AppState>) -> Result<(), String> {
    start_bridge_inner(&state).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
async fn stop_bridge(state: State<'_, AppState>) -> Result<(), String> {
    let handle = state.bridge.lock().unwrap().take();
    if let Some(handle) = handle {
        handle.token.cancel();
        let _ = handle.join.await;
    }
    emit_status(&state.app, StatusDto { kind: "stopped".into(), detail: "已停止".into(), streaming: false });
    update_tray_tooltip(&state.app, "已停止");
    Ok(())
}

// ---------------------------------------------------------------------------
// 桥接 / HID 编排
// ---------------------------------------------------------------------------

/// 按当前配置重启在场监管。任何影响它的设置改动之后都要调一次。
///
/// 全程只在同步锁里完成：旧句柄取消后自行收尾，不 await，因此不存在
/// "取消到重建之间被第二次调用插进来" 的窗口。
fn restart_presence(state: &AppState) {
    let settings = {
        let c = state.config.lock().unwrap();
        watch::Settings {
            bound_id: c.bound_device.as_ref().map(|d| d.id.clone()),
            keep_awake: c.keep_awake,
            auto_unlock: c.auto_unlock,
        }
    };
    let mut slot = state.presence.lock().unwrap();
    if let Some(old) = slot.take() {
        old.cancel();
    }
    *slot = Some(watch::start(state.app.clone(), settings));
}

/// 绑定变了要让桥接重新选设备。没在跑就什么都不做——下次启动自然带上新绑定。
fn restart_bridge_if_running(state: &AppState) {
    let Some(old) = state.bridge.lock().unwrap().take() else { return };
    old.token.cancel();
    let app = state.app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = old.join.await;
        let state = app.state::<AppState>();
        if let Err(e) = start_bridge_inner(&state) {
            log::warn!("绑定变更后重启桥接失败: {e:#}");
        }
    });
}

fn start_bridge_inner(state: &AppState) -> anyhow::Result<()> {
    if state.bridge.lock().unwrap().is_some() {
        return Ok(());
    }
    let (output, gain, fn_remap, bound_id) = {
        let c = state.config.lock().unwrap();
        (
            c.output_device.clone(),
            c.gain_db,
            c.fn_remap,
            c.bound_device.as_ref().map(|d| d.id.clone()),
        )
    };
    let output = output.ok_or_else(|| anyhow::anyhow!("尚未选择语音输出设备"))?;
    let sink = LoopbackSink::open(&output)?;
    let mut multi = MultiSink::new(vec![Box::new(sink)]);

    let token = CancellationToken::new();
    let opts = BridgeOptions { gain_db: gain, fn_remap, bound_id, ..Default::default() };
    let app = state.app.clone();
    let status_cb: StatusCallback = Arc::new(move |s: BridgeStatus| {
        // Windows：语音流边沿触发系统语音输入（Win+H 切换）。
        if let BridgeStatus::Streaming(active) = s {
            if cfg!(windows) {
                let enabled = app.state::<AppState>().config.lock().unwrap().win_hotkey;
                if enabled {
                    dictation::on_stream(active);
                }
            }
        }
        let dto = StatusDto::from(s);
        update_tray_tooltip(&app, &dto.detail);
        emit_status(&app, dto);
    });

    let child_token = token.clone();
    let join = tauri::async_runtime::spawn(async move {
        if let Err(e) = bridge::run(&mut multi, &opts, &child_token, Some(status_cb.clone())).await {
            let dto = StatusDto { kind: "error".into(), detail: format!("{e:#}"), streaming: false };
            status_cb(BridgeStatus::Error(dto.detail));
        }
    });

    *state.bridge.lock().unwrap() = Some(BridgeHandle { token, join });
    // 桥接启动时按配置同步 HID 映射。
    refresh_hid(state);
    Ok(())
}

/// 依据当前配置、前台应用与平台，启动 / 更新 / 停止按键映射。
///
/// 前台应用只影响"喂给 HID 层的是哪张表"——HID 层对应用无感知，切换应用就是
/// 一次热更新，和在界面上改键走的是同一条路径。
fn refresh_hid(state: &AppState) {
    #[cfg(target_os = "macos")]
    {
        let enabled = state.config.lock().unwrap().key_mapping;
        if !enabled {
            *state.hid.lock().unwrap() = None; // Drop 停止读取线程并恢复
            return;
        }
        let active = active_bundle_id(state);
        let map = state.config.lock().unwrap().app_key_maps().resolve(active.as_deref());
        let mut slot = state.hid.lock().unwrap();
        match slot.as_ref() {
            Some(h) => h.update_keymap(map),
            None => *slot = Some(HidMapper::start(map)),
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = state;
}

/// 注册前台应用监听（macOS）。只在按键映射**已在运行**时才热更新映射：
/// 保持"没启用映射就完全不介入系统"这一点不变。
#[cfg(target_os = "macos")]
fn watch_front_app(handle: &AppHandle) {
    let handle = handle.clone();
    let own_id = handle.config().identifier.clone();
    frontapp::watch(Arc::new(move |app| {
        let state = handle.state::<AppState>();
        let label = app.as_ref().map(|a| a.bundle_id.clone());
        let (switched, recent) = {
            let mut front = state.front.lock().unwrap();
            let switched = front.active.as_ref().map(|a| &a.bundle_id)
                != app.as_ref().map(|a| &a.bundle_id);
            front.active = app.clone();
            let recent = match app {
                Some(a) if a.bundle_id != own_id => {
                    front.recent = Some(a);
                    front.recent.clone()
                }
                _ => None,
            };
            (switched, recent)
        };
        if !switched {
            return;
        }
        log::debug!("前台应用：{}", label.as_deref().unwrap_or("(无)"));
        if state.hid.lock().unwrap().is_some() {
            refresh_hid(&state);
        }
        if let Some(recent) = recent {
            let _ = handle.emit("front-app", app_dto(&state, recent));
        }
    }));
}

fn emit_status(app: &AppHandle, dto: StatusDto) {
    let _ = app.emit("bridge-status", dto);
}

fn update_tray_tooltip(app: &AppHandle, text: &str) {
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(format!("RCTool · {text}")));
    }
}

// ---------------------------------------------------------------------------
// 应用装配
// ---------------------------------------------------------------------------

/// macOS：切换 Dock 图标可见性。Accessory 策略下应用只剩菜单栏图标，
/// 但托盘、后台桥接、按键映射都照常工作。其他平台是空操作。
fn set_dock_visible(app: &AppHandle, visible: bool) {
    #[cfg(target_os = "macos")]
    {
        use tauri::ActivationPolicy;
        let policy =
            if visible { ActivationPolicy::Regular } else { ActivationPolicy::Accessory };
        if let Err(e) = app.set_activation_policy(policy) {
            log::warn!("切换 Dock 图标失败: {e}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, visible);
}

/// 显示并聚焦主窗口（托盘菜单、Dock 点击、启动时共用）。
fn show_main_window(app: &AppHandle) {
    // 先把图标放回 Dock：Accessory 应用的窗口拿不到正常的前台焦点。
    set_dock_visible(app, true);
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_config,
            list_outputs,
            get_actions,
            get_buttons,
            set_binding,
            reset_bindings,
            list_running_apps,
            get_front_app,
            get_app_profiles,
            add_app_profile,
            remove_app_profile,
            set_app_profile_enabled,
            set_app_binding,
            clear_app_bindings,
            set_output,
            set_gain,
            set_fn_remap,
            set_win_hotkey,
            set_key_mapping,
            set_hide_dock_on_close,
            get_permissions,
            request_input_monitoring,
            request_accessibility,
            setup_loopback,
            start_bridge,
            stop_bridge,
            list_remotes,
            bind_device,
            unbind_device,
            set_keep_awake,
            set_auto_unlock,
            set_unlock_password,
            clear_unlock_password,
            set_launch_at_login,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            let config_path = handle
                .path()
                .app_config_dir()
                .map(|d| d.join("config.json"))
                .unwrap_or_else(|_| PathBuf::from("rctool-config.json"));
            let config = Config::load(&config_path);

            app.manage(AppState {
                config_path,
                config: Mutex::new(config),
                bridge: Mutex::new(None),
                presence: Mutex::new(None),
                #[cfg(target_os = "macos")]
                hid: Mutex::new(None),
                #[cfg(target_os = "macos")]
                front: Mutex::new(FrontState::default()),
                app: handle.clone(),
            });

            // 前台应用监听要在主线程注册；setup 就在主线程上。
            #[cfg(target_os = "macos")]
            watch_front_app(&handle);

            // 在场监管随应用启动，不等桥接——防睡眠/自动解锁不该被迫先配语音。
            restart_presence(&handle.state::<AppState>());

            // 应用可能被挪过位置（重装、换目录），对一下自启路径。
            #[cfg(target_os = "macos")]
            autostart::refresh_path_if_enabled();

            // 托盘菜单
            let settings_item = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
            let start_item = MenuItem::with_id(app, "start", "启用桥接", true, None::<&str>)?;
            let stop_item = MenuItem::with_id(app, "stop", "停用桥接", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "退出 RCTool", true, None::<&str>)?;
            let menu = MenuBuilder::new(app)
                .item(&settings_item)
                .separator()
                .item(&start_item)
                .item(&stop_item)
                .separator()
                .item(&quit_item)
                .build()?;

            let _tray = TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("RCTool · 已停止")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => show_main_window(app),
                    "start" => {
                        let state = app.state::<AppState>();
                        if let Err(e) = start_bridge_inner(&state) {
                            emit_status(
                                app,
                                StatusDto { kind: "error".into(), detail: format!("{e:#}"), streaming: false },
                            );
                        }
                    }
                    "stop" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let handle = app.state::<AppState>().bridge.lock().unwrap().take();
                            if let Some(h) = handle {
                                h.token.cancel();
                                let _ = h.join.await;
                            }
                            update_tray_tooltip(&app, "已停止");
                            emit_status(
                                &app,
                                StatusDto { kind: "stopped".into(), detail: "已停止".into(), streaming: false },
                            );
                        });
                    }
                    "quit" => {
                        // 退出前恢复 F5 映射：丢弃 HID + 桥接。
                        let state = app.state::<AppState>();
                        #[cfg(target_os = "macos")]
                        {
                            *state.hid.lock().unwrap() = None;
                        }
                        if let Some(h) = state.bridge.lock().unwrap().take() {
                            h.token.cancel();
                        }
                        if let Some(h) = state.presence.lock().unwrap().take() {
                            h.cancel();
                        }
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            // 首次启动打开设置窗，方便用户配置。
            show_main_window(&handle);
            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭主窗口只隐藏，应用继续驻留托盘（Dock 图标按设置去留）。
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                    let app = window.app_handle();
                    let hide_dock =
                        app.state::<AppState>().config.lock().unwrap().hide_dock_on_close;
                    if hide_dock {
                        set_dock_visible(app, false);
                    }
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("启动 RCTool 失败")
        .run(|app, event| {
            // macOS：点击 Dock 图标（窗口已隐藏时）重新显示主窗口。
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                show_main_window(app);
            }
            let _ = (app, &event);
        });
}
