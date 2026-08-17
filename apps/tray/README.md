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
应用继续驻留（Dock/托盘）：点 Dock 图标（macOS Reopen）或托盘「显示主窗口」
即可重新唤出。这是**主窗口 + 托盘并存**形态，不是纯菜单栏工具。

## 打包（.app / .dmg）

需要 tauri-cli：

```bash
cd apps/tray
pnpm dlx @tauri-apps/cli@2 build
```

产物在 `src-tauri/target/release/bundle/`。macOS 分发还需 Developer ID 签名
与公证（见根 README 路线图）。

## 结构

```
src/                     纯静态前端（无打包器）
  index.html             三个标签页：连接 / 按键 / 权限
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
| 按键映射（拦截/注入） | ✅ 连接/按键/权限三页 | 未实现（Win 禁止用户态 raw 读键盘 HID） | 未实现 |

macOS 显示「连接/按键/权限」三页；Windows 显示「连接」页并多一个 Win+H 开关；
Linux 只显示「连接」页。这些差异全在前端按 `platform` 切换，后端命令三平台一致。

## 设置页

| 标签 | 内容 |
| --- | --- |
| 连接 | 语音输出设备选择（标注回环设备）、增益、听写触发开关、启用/停用桥接 |
| 按键 | （仅 macOS）启用按键映射开关；12 个实体键逐个下拉选择动作；恢复默认 |
| 权限 | （仅 macOS）输入监控 / 辅助功能状态与请求入口 |

配置改动即时保存并热生效：改键无需重启读取线程（`HidMapper::update_keymap`），
换输出设备下次启用桥接时生效。

## 后端命令

`get_config` `list_outputs` `get_actions` `get_buttons` `set_binding`
`reset_bindings` `set_output` `set_gain` `set_fn_remap` `set_key_mapping`
`get_permissions` `request_permissions` `start_bridge` `stop_bridge`

桥接状态通过 `bridge-status` 事件推送给前端，同时更新托盘 tooltip。

## 已知边界（待真机验证）

与 core 相同：BLE 连接、ATVV 语音、F5→Fn 听写、HID 拦截/注入均需接真实
RC003 遥控器验证。本应用在无设备时启动、显示 UI、读写配置、枚举音频设备
均已验证可用。
