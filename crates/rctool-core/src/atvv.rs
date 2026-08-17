//! Android TV Voice-over-BLE（ATVV）协议常量与报文编解码。
//!
//! 这只遥控器的语音不走任何蓝牙标准音频协议（它是 BLE-only 设备，没有
//! Classic 的 HFP；LE Audio 在其设计年代尚不存在）。语音是 Google 为
//! Android TV 遥控器定义的私有 GATT 协议，主机侧（本程序）扮演电视的角色：
//!
//! ```text
//! 主机                                遥控器
//!  │── TX: GET_CAPS ──────────────────▶│
//!  │◀── CTL: CAPS_RESP (0x0B) ─────────│  版本 / 编码 / 帧长
//!  │◀── CTL: START_SEARCH (0x08) ──────│  用户按下麦克风键
//!  │── TX: MIC_OPEN (0x0C) ───────────▶│
//!  │◀── CTL: AUDIO_START (0x04) ───────│
//!  │◀── AUDIO: ADPCM 帧流（notify）────│  16 kHz IMA ADPCM
//!  │◀── CTL: AUDIO_SYNC (0x0A) ────────│  丢包后重置解码器状态
//!  │◀── CTL: AUDIO_STOP (0x00) ────────│  用户松开麦克风键
//! ```

use uuid::Uuid;

/// ATVV 语音服务。
pub const SERVICE: Uuid = Uuid::from_u128(0xAB5E0001_5A21_4F05_BC7D_AF01F617B664);
/// 主机 → 遥控器 命令通道（write）。
pub const CHAR_TX: Uuid = Uuid::from_u128(0xAB5E0002_5A21_4F05_BC7D_AF01F617B664);
/// 遥控器 → 主机 音频数据（notify）。
pub const CHAR_AUDIO: Uuid = Uuid::from_u128(0xAB5E0003_5A21_4F05_BC7D_AF01F617B664);
/// 遥控器 → 主机 控制事件（notify）。
pub const CHAR_CTL: Uuid = Uuid::from_u128(0xAB5E0004_5A21_4F05_BC7D_AF01F617B664);

/// 16-bit 标准蓝牙 UUID → 128-bit（Bluetooth Base UUID）。
pub const fn bt16(short: u16) -> Uuid {
    Uuid::from_u128(0x0000_0000_0000_1000_8000_0080_5F9B_34FB | ((short as u128) << 96))
}

/// Device Information Service，读型号用（RC003 / ARN9）。
pub const DEVICE_INFORMATION_SERVICE: Uuid = bt16(0x180A);
/// Model Number String。
pub const MODEL_NUMBER_CHARACTERISTIC: Uuid = bt16(0x2A24);

/// GET_CAPS（v1.0 布局）：主机声明支持 8k/16k ADPCM。
pub const GET_CAPS: [u8; 6] = [0x0A, 0x01, 0x00, 0x00, 0x03, 0x03];

/// MIC_OPEN：v1.0 之后 codec 在 AUDIO_START 里协商，报文不再携带。
pub fn mic_open(version: u16, codec: u8) -> Vec<u8> {
    if version >= 0x0100 {
        vec![0x0C, 0x00]
    } else {
        vec![0x0C, 0x00, codec]
    }
}

/// MIC_CLOSE：v1.0 携带会话 ID。
pub fn mic_close(version: u16, session_id: u8) -> Vec<u8> {
    if version >= 0x0100 {
        vec![0x0D, session_id]
    } else {
        vec![0x0D]
    }
}

/// CAPS_RESP（0x0B）解析结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub version: u16,
    pub codecs: u8,
    /// 音频特征上攒满多少字节算一帧 ADPCM。
    pub frame_size: usize,
    /// 0x01 = 8 kHz，0x02 = 16 kHz。
    pub codec: u8,
    pub sample_rate: u32,
}

impl Caps {
    /// RC003 真机实测默认值：16 kHz ADPCM、120 字节/帧。
    pub const DEFAULT: Caps = Caps {
        version: 0x0100,
        codecs: 0x02,
        frame_size: 120,
        codec: 0x02,
        sample_rate: 16_000,
    };

    /// 解析 0x0B 能力响应。兼容 v0.4 旧布局，以及部分固件在 v1.0 布局下
    /// 把 codec 位错放进下一个字节的怪癖（原 macOS 实现真机踩出的坑）。
    pub fn parse(data: &[u8]) -> Option<Caps> {
        if data.len() < 7 || data[0] != 0x0B {
            return None;
        }
        let version = u16::from_be_bytes([data[1], data[2]]);
        let codecs = if version >= 0x0100 {
            let mut c = data[3];
            if c == 0 && data.len() >= 9 && data[4] & 0x03 != 0 {
                c = data[4];
            }
            c
        } else {
            if data.len() < 9 {
                return None;
            }
            data[4]
        };
        let frame_size = ((data[5] as usize) << 8) | data[6] as usize;
        let codec: u8 = if codecs & 0x02 != 0 { 0x02 } else { 0x01 };
        Some(Caps {
            version,
            codecs,
            frame_size: if frame_size == 0 { 120 } else { frame_size },
            codec,
            sample_rate: if codec == 0x02 { 16_000 } else { 8_000 },
        })
    }

    /// AUDIO_START 可以临时改选编码（进而改变采样率）。
    pub fn with_stream_codec(self, codec: u8) -> Caps {
        Caps {
            codec,
            sample_rate: if codec == 0x02 { 16_000 } else { 8_000 },
            ..self
        }
    }
}

/// CTL 特征上的控制事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlEvent {
    /// 0x0B：能力响应（原始字节交给 [`Caps::parse`]）。
    CapsResp,
    /// 0x08：遥控器请求主机打开它的麦克风（用户按下语音键）。
    StartSearch,
    /// 0x04：语音流开始。
    AudioStart { codec: Option<u8>, session_id: Option<u8> },
    /// 0x00：语音流结束。
    AudioStop,
    /// 0x0A：丢包重同步，携带解码器 predictor 与 step index。
    AudioSync { predictor: i16, step_index: u8 },
    /// 未识别的 opcode。
    Other(u8),
}

impl ControlEvent {
    pub fn parse(data: &[u8]) -> Option<ControlEvent> {
        match *data.first()? {
            0x0B => Some(Self::CapsResp),
            0x08 => Some(Self::StartSearch),
            0x04 => Some(Self::AudioStart {
                codec: data.get(2).copied(),
                session_id: data.get(3).copied(),
            }),
            0x00 => Some(Self::AudioStop),
            0x0A => {
                if data.len() < 7 {
                    return None;
                }
                Some(Self::AudioSync {
                    predictor: i16::from_be_bytes([data[4], data[5]]),
                    step_index: data[6],
                })
            }
            op => Some(Self::Other(op)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_v1_layout() {
        let caps = Caps::parse(&[0x0B, 0x01, 0x00, 0x02, 0x03, 0x00, 0x78]).unwrap();
        assert_eq!(caps.version, 0x0100);
        assert_eq!(caps.codec, 0x02);
        assert_eq!(caps.sample_rate, 16_000);
        assert_eq!(caps.frame_size, 120);
    }

    #[test]
    fn caps_v1_codec_bits_in_next_byte_quirk() {
        let caps = Caps::parse(&[0x0B, 0x01, 0x00, 0x00, 0x02, 0x00, 0x78, 0x00, 0x00]).unwrap();
        assert_eq!(caps.codec, 0x02);
        assert_eq!(caps.sample_rate, 16_000);
    }

    #[test]
    fn caps_legacy_v04_layout() {
        let caps = Caps::parse(&[0x0B, 0x00, 0x04, 0x00, 0x03, 0x00, 0x86, 0x00, 0x00]).unwrap();
        assert_eq!(caps.version, 0x0004);
        assert_eq!(caps.codec, 0x02);
        assert_eq!(caps.frame_size, 134);
    }

    #[test]
    fn caps_zero_frame_size_falls_back_to_default() {
        let caps = Caps::parse(&[0x0B, 0x01, 0x00, 0x02, 0x03, 0x00, 0x00]).unwrap();
        assert_eq!(caps.frame_size, 120);
    }

    #[test]
    fn caps_rejects_wrong_opcode_and_short_payload() {
        assert!(Caps::parse(&[0x0C, 0x01, 0x00, 0x02, 0x03, 0x00, 0x78]).is_none());
        assert!(Caps::parse(&[0x0B, 0x01, 0x00]).is_none());
    }

    #[test]
    fn mic_open_close_layouts() {
        assert_eq!(mic_open(0x0100, 0x02), vec![0x0C, 0x00]);
        assert_eq!(mic_open(0x0004, 0x02), vec![0x0C, 0x00, 0x02]);
        assert_eq!(mic_close(0x0100, 7), vec![0x0D, 0x07]);
        assert_eq!(mic_close(0x0004, 7), vec![0x0D]);
    }

    #[test]
    fn control_event_parsing() {
        assert_eq!(ControlEvent::parse(&[0x08]), Some(ControlEvent::StartSearch));
        assert_eq!(
            ControlEvent::parse(&[0x04, 0x00, 0x02, 0x09]),
            Some(ControlEvent::AudioStart { codec: Some(0x02), session_id: Some(9) })
        );
        assert_eq!(ControlEvent::parse(&[0x00]), Some(ControlEvent::AudioStop));
        assert_eq!(
            ControlEvent::parse(&[0x0A, 0, 0, 0, 0x00, 0x64, 0x05]),
            Some(ControlEvent::AudioSync { predictor: 100, step_index: 5 })
        );
        assert_eq!(ControlEvent::parse(&[0x0A, 0, 0]), None);
        assert_eq!(ControlEvent::parse(&[0xEE]), Some(ControlEvent::Other(0xEE)));
    }
}
