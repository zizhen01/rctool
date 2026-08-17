//! # rctool-core
//!
//! Xiaomi Bluetooth Remote 2 Pro（RC003）/ 普通款（ARN9）语音遥控器的
//! 桥接核心。遥控器的按键是标准 HID over GATT（系统原生支持），语音则是
//! Google 的 Android TV Voice-over-BLE（ATVV）私有 GATT 协议——桌面系统
//! 不认识它，本库在桌面侧实现 Android TV 的 host 角色，把语音解成 PCM
//! 并送进用户选定的音频出口。
//!
//! 分层（自下而上）：
//!
//! - [`atvv`]：协议常量、报文编解码（纯函数）
//! - [`adpcm`]：IMA/DVI ADPCM 解码器（纯逻辑）
//! - [`dsp`]：帧组装、平滑/增益、重采样（纯逻辑）
//! - [`session`]：单连接会话状态机（纯逻辑，注入时间，全部可单测）
//! - [`sink`]：PCM 出口抽象（wav / 扇出 / 将来 ASR）
//! - [`loopback`]：cpal 回环输出（BlackHole / VB-Cable / null-sink）
//! - [`bridge`]：bluest BLE 发现、连接、事件泵与断线重连
//!
//! 与原 Swift 实现的最大结构差异：不需要 generation gating。会话与连接
//! 同生共死（所有权），旧连接的回调不可能投递到新会话。

pub mod adpcm;
pub mod atvv;
pub mod bridge;
pub mod dsp;
pub mod fnmap;
pub mod loopback;
pub mod session;
pub mod sink;

/// 供上层（CLI / Tauri 壳）复用同版本依赖。
pub use bluest;
pub use tokio_util;
