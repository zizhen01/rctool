//! RCTool 托盘应用后端。
//!
//! 职责：管理配置、桥接（BLE→回环）生命周期、按键映射（macOS），并把状态
//! 推给设置界面与托盘。核心逻辑全部来自 `rctool-core`，这里只做编排与 UI。

mod config;

use config::Config;
use rctool_core::bridge::{self, BridgeOptions, BridgeStatus, StatusCallback};
use rctool_core::keymap::{Action, RemoteButton};
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
use rctool_core::hidmap::{HidMapper, Permissions};

struct BridgeHandle {
    token: CancellationToken,
    join: tauri::async_runtime::JoinHandle<()>,
}

struct AppState {
    config_path: PathBuf,
    config: Mutex<Config>,
    bridge: Mutex<Option<BridgeHandle>>,
    #[cfg(target_os = "macos")]
    hid: Mutex<Option<HidMapper>>,
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
    key_mapping: bool,
    running: bool,
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
        key_mapping: c.key_mapping,
        running: state.bridge.lock().unwrap().is_some(),
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
fn set_key_mapping(state: State<AppState>, enabled: bool) {
    state.config.lock().unwrap().key_mapping = enabled;
    state.save_config();
    refresh_hid(&state);
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

#[tauri::command]
fn request_permissions() {
    #[cfg(target_os = "macos")]
    {
        Permissions::request_input_monitoring();
        // 辅助功能没有静默请求 API：打开系统设置对应面板。
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .spawn();
    }
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

fn start_bridge_inner(state: &AppState) -> anyhow::Result<()> {
    if state.bridge.lock().unwrap().is_some() {
        return Ok(());
    }
    let (output, gain, fn_remap) = {
        let c = state.config.lock().unwrap();
        (c.output_device.clone(), c.gain_db, c.fn_remap)
    };
    let output = output.ok_or_else(|| anyhow::anyhow!("尚未选择语音输出设备"))?;
    let sink = LoopbackSink::open(&output)?;
    let mut multi = MultiSink::new(vec![Box::new(sink)]);

    let token = CancellationToken::new();
    let opts = BridgeOptions { gain_db: gain, fn_remap, ..Default::default() };
    let app = state.app.clone();
    let status_cb: StatusCallback = Arc::new(move |s: BridgeStatus| {
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

/// 依据当前配置与平台，启动 / 更新 / 停止按键映射。
fn refresh_hid(state: &AppState) {
    #[cfg(target_os = "macos")]
    {
        let (enabled, map) = {
            let c = state.config.lock().unwrap();
            (c.key_mapping, c.key_map())
        };
        let mut slot = state.hid.lock().unwrap();
        if enabled {
            match slot.as_ref() {
                Some(h) => h.update_keymap(map),
                None => *slot = Some(HidMapper::start(map)),
            }
        } else {
            *slot = None; // Drop 停止读取线程并恢复
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = state;
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

fn show_settings(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.show();
        let _ = win.set_focus();
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            get_config,
            list_outputs,
            get_actions,
            get_buttons,
            set_binding,
            reset_bindings,
            set_output,
            set_gain,
            set_fn_remap,
            set_key_mapping,
            get_permissions,
            request_permissions,
            start_bridge,
            stop_bridge,
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
                #[cfg(target_os = "macos")]
                hid: Mutex::new(None),
                app: handle.clone(),
            });

            // 托盘菜单
            let settings_item = MenuItem::with_id(app, "settings", "打开设置…", true, None::<&str>)?;
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
                    "settings" => show_settings(app),
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
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            // 首次启动打开设置窗，方便用户配置。
            show_settings(&handle);
            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭设置窗只隐藏，应用继续驻留托盘。
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "settings" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("启动 RCTool 失败");
}
