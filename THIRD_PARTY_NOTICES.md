# 第三方组件声明

## BlackHole（仅 macOS full 版内嵌）

macOS **full** 发行版内嵌未修改的 BlackHole 官方安装器
（`BlackHole2ch-<version>.pkg`），用于应用内一键安装虚拟音频设备。

- 项目：<https://github.com/ExistentialAudio/BlackHole>
- 版权：© Existential Audio Inc.
- 许可证：GPL-3.0（与本项目一致）
- 内嵌版本与校验和见 `.github/workflows/release.yml` 中的
  “Fetch BlackHole installer” 步骤；对应源代码可在上述上游仓库按版本
  标签获取。

lite 版不包含任何第三方安装器，仅提供指向官方渠道的安装引导。

## VB-Cable（不内嵌）

Windows 版**不包含** VB-Cable（VB-Audio Software，闭源 donationware，
许可证不允许未授权再分发）。应用仅在用户明确点击时从 VB-Audio 官方
服务器下载其安装器到用户设备并启动，等同于用户手动操作。

- 官网：<https://vb-audio.com/Cable/>

## Rust 依赖

Rust crate 依赖的许可证信息可通过 `cargo license` 或各 crate 的
crates.io 页面查询，均为与 GPL-3.0 兼容的宽松许可证（MIT/Apache-2.0 等）。
