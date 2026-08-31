# RCTool 托盘应用

基于 Tauri v2 的菜单栏应用，把 `rctool-core` 的语音桥接与按键映射包进一个
托盘图标 + 设置窗口。核心逻辑全在 core crate，本目录只做编排与 UI。

## 运行（开发）

前端是纯静态文件（`src/`，无构建步骤），所以直接用 cargo 即可启动整个应用，
不需要 Node 或 tauri-cli：

```bash
cargo run -p rctool-tray
```

启动后有 Dock 图标 + 菜单栏托盘图标，主窗口自动显示。关闭主窗口只是隐藏，
应用继续驻留：托盘「显示主窗口」随时唤回。macOS 上默认还会在关窗时把图标移出
Dock（切到 Accessory 激活策略），只留菜单栏图标——「连接」页的「关闭窗口时移出
Dock」开关可以关掉，关掉后 Dock 图标常驻，点 Dock 图标（macOS Reopen）也能唤回
窗口。桥接与按键映射不受这个开关影响。

## tauri dev / 打包

不装任何东西的日常开发就是 `cargo run -p rctool-tray`。想用 tauri-cli 的
工作流（Rust 改动自动重启、前端改动自动刷新 webview）：

```bash
cd apps/tray
npx --yes @tauri-apps/cli@2 dev
```

打包（.app/.dmg、.msi/.nsis、.deb/.AppImage 按平台）：

```bash
cd apps/tray
npx --yes @tauri-apps/cli@2 build
```

产物在仓库根 `target/release/bundle/`。调试小抄：debug 构建里网页右键 →
“检查元素”可开 devtools；`src-tauri/Info.plist` 会被合并进打包版（蓝牙用途
声明在此，缺了打包版会被 TCC 终止）；`Info.dev.plist` 只嵌进 `cargo run`
的裸二进制。macOS 正式分发还需 Developer ID 签名与公证。

## 结构

```
src/                     纯静态前端（无打包器）
  index.html             四个标签页：连接 / 按键 / 应用 / 权限
  styles.css             明暗自适应
  main.js                通过 window.__TAURI__ 调用后端命令
src-tauri/
  src/lib.rs             状态、命令、托盘、桥接与 HID 生命周期编排
  src/config.rs          配置持久化（app config dir 下 config.json）
  tauri.conf.json        窗口 / 托盘 / 打包配置
  capabilities/          最小权限集
  Info.dev.plist         dev 运行时嵌入的蓝牙用途声明（+ LSUIElement）
```

## 三平台

界面按平台自适应（`get_config` 返回 `platform`，前端裁剪）：

| 能力 | macOS | Windows | Linux |
| --- | --- | --- | --- |
| BLE 语音 → 虚拟麦克风 | ✅ BlackHole | ✅ VB-Cable | ✅ null-sink |
| 听写触发 | F5→Fn 设备级重映射（按住即听写） | 语音流边沿合成 Win+H（切换语音输入） | 无系统级听写，仅提供虚拟麦克风 |
| 按键映射（拦截/注入） | ✅ 连接/按键/应用/权限四页 | 未实现（Win 禁止用户态 raw 读键盘 HID） | 未实现 |
| 按应用覆盖映射 | ✅ NSWorkspace 前台应用通知 | 未实现 | 未实现 |

macOS 显示「连接/按键/应用/权限」四页；Windows 显示「连接」页并多一个 Win+H
开关；Linux 只显示「连接」页。这些差异全在前端按 `platform` 切换，后端命令三平台一致。

## 设置页

| 标签 | 内容 |
| --- | --- |
| 连接 | 语音输出设备选择（标注回环设备）、增益、听写触发开关、关闭窗口时移出 Dock（仅 macOS）、启用/停用桥接 |
| 按键 | （仅 macOS）启用按键映射开关；12 个实体键逐个下拉选择动作；恢复默认 |
| 应用 | （仅 macOS）按前台应用的覆盖层：只列出与「按键」页全局映射的差异 |
| 权限 | （仅 macOS）输入监控 / 辅助功能状态与请求入口 |

配置改动即时保存并热生效：改键无需重启读取线程（`HidMapper::update_keymap`），
换输出设备下次启用桥接时生效。

「应用」页刻意不复制「按键」页那张整表——一个应用值得单独记住的，就是它和全局
差在哪几个键。覆盖层只存差量，所以全局改了某个键，没有专门覆盖它的应用会跟着
变。前台应用切换（`rctool_core::frontapp` 的 NSWorkspace 通知，主线程注册）只是
把解析出的那张表热更新给 HID 层——HID 层对"哪个应用"毫不知情，和界面上改键走的
是同一条路径。映射未启用时不介入：前台变化只在读取线程已在跑时才触发热更新。

## 后端命令

`get_config` `list_outputs` `get_actions` `get_buttons` `set_binding`
`reset_bindings` `set_output` `set_gain` `set_fn_remap` `set_key_mapping`
`get_permissions` `request_input_monitoring` `request_accessibility` `start_bridge` `stop_bridge`

按应用覆盖：`list_running_apps` `get_front_app` `get_app_profiles`
`add_app_profile` `remove_app_profile` `set_app_profile_enabled`
`set_app_binding` `clear_app_bindings`

桥接状态通过 `bridge-status` 事件推送给前端，同时更新托盘 tooltip；前台应用变化
通过 `front-app` 事件推送（用于「应用」页的"生效中"标记与快捷添加）。

## 已知边界（待真机验证）

与 core 相同：BLE 连接、ATVV 语音、F5→Fn 听写、HID 拦截/注入均需接真实
RC003 遥控器验证。本应用在无设备时启动、显示 UI、读写配置、枚举音频设备
均已验证可用。
