# RCTool

小米蓝牙语音遥控器（Bluetooth Remote 2 Pro / **RC003**、普通款 / **ARN9**）的桌面桥接工具，Rust 重写版。

遥控器的按键是标准 HID over GATT，配对后系统原生可用；语音则是 Google 的
**ATVV（Android TV Voice-over-BLE）** 私有 GATT 协议，桌面系统不认识。本工具在
桌面侧扮演 Android TV 的 host 角色：完成 ATVV 握手，把 16 kHz IMA ADPCM 语音流
解码成 PCM，写入回环音频设备（BlackHole 等），其输入侧即成为系统里的"虚拟麦克
风"，供系统听写 / 输入法语音使用。

```
RC003 ──BLE GATT/ATVV──▶ rctool ──PCM──▶ BlackHole ──▶ 系统听写 / 输入法
        （按住麦克风键）      解码            回环         从"BlackHole"收音
```

## 状态

- ✅ 编译通过（macOS，Rust 1.97），核心逻辑 25 个单元测试全绿
- ✅ macOS F5→Fn/🌐 设备级重映射（`hidutil UserKeyMapping` 机制，仅作用于遥控器）：
  按住麦克风键 = 按住 Fn，系统听写自动开始/结束；退出与断开自动恢复原映射
- ✅ macOS 按键映射（监听 + 关联拦截 + 注入）：规避 TV 反引号 / 电源关机对话框，
  救活返回键（0xF1），12 键全可配置；恒等映射自动直通零开销
- ✅ Tauri v2 菜单栏应用（`apps/tray`）：托盘图标 + 设置窗，配置持久化、状态实时推送、
  改键热生效；界面按平台自适应（mac/win/linux），三平台交叉编译通过、CI 覆盖
- ✅ Windows 听写触发（语音流边沿合成 Win+H）、Linux 虚拟麦克风（null-sink）
- ✅ 协议层与解码器移植自已通过真机验收的 open-voice-bridge 实现（含其真机踩坑：
  AUDIO_START 竞态、8 kHz 回退拒绝、AUDIO_SYNC 重同步、能力响应固件怪癖）
- ⏳ **本仓库代码尚未真机验收**：需要配对真实 RC003/ARN9 后跑通全链路
- ⏳ Windows / Linux：依赖（bluest/cpal）跨平台，未在真机验证
- 计划中：Tauri v2 tray 壳、按键自定义映射层

## 两种用法

**菜单栏应用**（推荐，图形化配置）：

```bash
cargo run -p rctool-tray
```

菜单栏出现 RCTool 图标并打开设置窗。详见 [apps/tray/README.md](apps/tray/README.md)。

**命令行**：

```bash
cargo build --release
```

三个子命令：

```bash
# 1. 列出音频输出设备（确认回环设备已装好）
./target/release/rctool outputs

# 2. 查找遥控器（先查系统已连接设备，再扫广播）
./target/release/rctool scan

# 3. 运行桥接（按住遥控器麦克风键说话）
./target/release/rctool run --output BlackHole

# 调试：不装回环设备也可以先把语音落成 wav 验证链路
./target/release/rctool run --wav test.wav
```

`run` 默认同时把遥控器的 F5（麦克风键）设备级重映射为 Fn/🌐（`--no-fn-remap`
关闭）；映射只影响遥控器这一台设备，进程退出或设备断开时自动恢复。

## macOS 首次使用

1. **配对遥控器**：系统设置 → 蓝牙；遥控器长按 主页+菜单 进入配对模式，
   设备名为 `MI RC`。日常使用时遥控器作为系统 HID 已连接、不再广播，
   rctool 通过"按服务检索已连接设备"找到它（这正是选 bluest 而非 btleplug
   的原因：后者没有这条 API）。
2. **安装回环驱动**：[BlackHole 2ch](https://existential.audio/blackhole/)。
3. **蓝牙权限**：从 Terminal/iTerm 首次运行 `rctool scan` 时，macOS 会请求终端的
   蓝牙权限，允许即可。二进制已通过 `__TEXT,__info_plist` 嵌入
   `NSBluetoothAlwaysUsageDescription`（裸二进制缺它会被 TCC 直接 SIGABRT，
   见 `crates/rctool-cli/build.rs`）。若未弹框且闪退，在
   系统设置 → 隐私与安全性 → 蓝牙 里手动加入你的终端应用。
4. **接上听写**：系统设置 → 键盘 → 听写：开启，快捷键选「按住 🌐」，
   麦克风来源选 BlackHole 2ch。之后按住遥控器麦克风键即开始听写（rctool
   已把该设备的 F5 重映射为 🌐），松开即结束。

## 仓库结构

```
crates/rctool-core/         核心库（无 UI 依赖，CLI 与 Tauri 壳共用）
  ├── atvv.rs               ATVV 协议常量与报文编解码（纯函数）
  ├── adpcm.rs              IMA/DVI ADPCM 解码器
  ├── dsp.rs                帧组装、平滑/增益、线性重采样
  ├── session.rs            单连接会话状态机（纯逻辑、注入时间、全可单测）
  ├── keymap.rs             按键→动作模型 + 默认表 + 恒等直通判定（纯逻辑）
  ├── sink.rs               AudioSink trait + wav/扇出实现
  ├── loopback.rs           cpal 回环输出（专用音频线程 + 无锁环形缓冲）
  ├── fnmap.rs              macOS F5→Fn/🌐 设备级重映射（IOHID FFI，退出恢复）
  ├── hidmap.rs             macOS 按键读取+拦截+注入（IOHIDManager/CGEvent FFI）
  └── bridge.rs             bluest 发现/连接/事件泵/断线重连
crates/rctool-cli/          rctool 命令行（含 macOS Info.plist 嵌入）
apps/tray/                  Tauri v2 菜单栏应用（前端纯静态，无构建步骤）
```

### 按键映射设计

配对后系统对方向/OK/音量键的原生行为就是对的，只有电源（会弹关机对话框）、
TV（打出反引号）、返回（系统层死键 0xF1）需要处理。因此不独占设备（独占会
废掉 F5→Fn 听写触发），而是：非独占读取 HID 边沿 → 对被改键的按钮用 CGEventTap
按时序关联抵消其原生事件 → 注入映射动作。**恒等映射自动折叠为直通**，默认只有
5 个键真正进入拦截路径，其余零开销原生放行。

设计要点：

- **会话与连接同生共死**。原 Swift 实现里大量 generation gating（防旧连接回调
  串台）由 Rust 所有权模型天然替代，不再需要。
- **PCM 出口是 trait**（`AudioSink`）。将来 ASR 直出文字、录音、电平诊断都是
  加实现，核心链路不动。识别永远不进桥接器：系统听写/输入法已有一流中文 ASR。
- **识别路线不自带模型**：不集成 whisper 等本地 ASR（体积/中文效果/流式形态
  都不合适）；若将来做"直出文字"，优先系统 ASR（macOS Speech framework /
  Windows WinRT），或按需下载 sherpa-onnx 系模型。

## 协议速查

```
Service  AB5E0001-5A21-4F05-BC7D-AF01F617B664
  ├─ AB5E0002  TX     主机→遥控器 写命令
  ├─ AB5E0003  AUDIO  遥控器→主机 音频流（notify）
  └─ AB5E0004  CTL    遥控器→主机 控制事件（notify）

主机写 GET_CAPS(0x0A) → 0x0B 能力响应（版本/编码/帧长）
按下麦克风键 → 0x08 START_SEARCH → 主机写 MIC_OPEN(0x0C)
→ 0x04 AUDIO_START → AUDIO 特征上 120B/帧 ADPCM（16 kHz，64 kbps）
→ 0x0A AUDIO_SYNC（丢包重同步）… → 松开 → 0x00 AUDIO_STOP
```

## 许可证

GPL-3.0-only。协议细节与解码逻辑源自
[nijez/open-voice-bridge](https://github.com/nijez/open-voice-bridge)（GPL-3.0）
的 macOS 实现及其真机验收结论。
