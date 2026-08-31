<p align="center">
  <img src="docs/poster.svg" alt="RCTool：把小米蓝牙语音遥控器变成桌面麦克风" width="100%">
</p>

<p align="center">中文 | <a href="README.en.md">English</a></p>

**当前适配：**

- 小米蓝牙遥控器 v2（RC001）
- 小米蓝牙遥控器 v2 Pro（RC003）

市面遥控器种类繁多，欢迎提交 PR 适配更多型号。

## 快速开始（macOS）

1. 系统蓝牙里配对遥控器：长按 主页+菜单 进入配对，设备名 `MI RC`
2. 启动应用：`cargo run -p rctool-tray`，或从 Releases 下载 dmg
3. 连接页选择输出设备 **BlackHole 2ch**（未安装时应用内有「安装 BlackHole」一键引导；full 版 dmg 已内置安装器）
4. 系统设置 → 键盘 → 听写：开启，快捷键选「按住 🌐」，麦克风来源选 BlackHole 2ch
5. 按住遥控器麦克风键说话，松开结束

按键映射（可选）：设置窗「按键」页点实物图改键，需要输入监控与辅助功能权限。
想让某个应用用另一套键（比如播放器里 OK 改成空格），到「应用」页给它加一层覆盖，
只写与全局不同的那几个键，切到该应用时自动生效。

## 防睡眠与自动解锁

遥控器在蓝牙范围内时，可以让 Mac 不进闲置睡眠，锁上的屏幕也自动解开。常年开着的
Mac mini 用这个保持 SSH 和屏幕共享一直可达。

到「设备」页：

1. 扫描并绑定遥控器。绑定后语音桥接和在场检测都只认这一台，旁边同型号的不会被误连
2. 打开「遥控器在场时阻止睡眠」。持有的是 `PreventUserIdleSystemSleep` 断言，
   `pmset -g assertions` 里查得到；你主动选睡眠、按电源键仍然照睡
3. 想连锁屏也省掉，填一次登录密码（存进钥匙串）再打开「锁屏时自动解锁」，
   需要辅助功能权限
4. 无头机记得再去「连接」页打开「开机自动启动」，否则重启后这些都不生效

自动解锁和 Apple 的 Auto Unlock 不是一回事：密码由合成键盘事件敲进登录窗，能仿冒
你遥控器的人就能解开这台 Mac。默认关闭，打开时会再确认一次。

在场判断看的是系统蓝牙连接状态，不是信号强度，分辨率只到「连着 / 没连着」。遥控器
搁在桌上没人也算在场。

## 用法

```bash
# 图形界面：主窗口 + 菜单栏托盘
cargo run -p rctool-tray

# 命令行
cargo run -p rctool-cli --release -- outputs            # 列音频输出设备
cargo run -p rctool-cli --release -- scan               # 查找遥控器
cargo run -p rctool-cli --release -- run --output BlackHole   # 运行桥接
cargo run -p rctool-cli --release -- run --wav test.wav       # 调试：语音落 wav
```

## 平台

| 平台 | 语音 → 虚拟麦克风 | 听写触发 | 按键映射 | 防睡眠 / 自动解锁 |
| --- | --- | --- | --- | --- |
| macOS | ✅ BlackHole（full 版内置安装器 / lite 版 brew·官网引导） | 按住麦克风键即听写（F5→Fn 设备级重映射） | ✅ 13 键图形化映射 ＋ 按应用覆盖 | ✅ 电源断言 ＋ 锁屏键入 ＋ 开机自启 |
| Windows | ✅ VB-Cable（应用内从官方源一键获取） | 语音时自动 Win+H | 暂未实现 | 暂未实现 |
| Linux | ✅ 一键创建 null-sink，零下载 | 无系统听写，仅虚拟麦克风 | 暂未实现 | 暂未实现 |

## 开发

```bash
just --list                      # 全部命令（装 just：brew install just）
just run                         # 起 GUI（前端纯静态，无 node 依赖）
just ci                          # 与 CI 同样的三步：测试 + 全量编译 + clippy
just install                     # 打包并装到 /Applications
just deploy minits               # 打包并部署到远程 Mac
just dist                        # 本机全套产物到 dist/（macOS 出 full/lite dmg + app.zip + CLI）
git tag v0.1.0 && git push --tags  # CI 出三平台安装包（Release 草稿）
```

macOS 上尽量用证书签名（`just sign-id` 会自动选本机唯一那张）。ad-hoc 签名的
designated requirement 就是二进制哈希，每次重编都变，于是系统权限每次重装都要重给。

细节文档：[apps/tray/README.md](apps/tray/README.md)（架构、三平台差异、打包）、
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)（full 版内嵌 BlackHole 的许可说明）。

## 许可证

GPL-3.0-only。协议细节与解码逻辑源自 [nijez/open-voice-bridge](https://github.com/nijez/open-voice-bridge)（GPL-3.0）。
