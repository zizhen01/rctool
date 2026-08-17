//! 单条 BLE 连接内的 ATVV 会话状态机。
//!
//! 纯逻辑、无 I/O、时间由调用方注入（`now_ms`），因此协议边界全部可以用
//! 普通单元测试覆盖。一条连接对应一个实例：断开即整体丢弃。原 Swift 实现
//! 里大量 generation gating（防旧连接回调串台）在这里被所有权模型天然替代
//! ——会话随连接一起销毁，不存在"上一代回调"这回事。

use crate::adpcm::ImaAdpcmDecoder;
use crate::atvv::{self, Caps, ControlEvent};
use crate::dsp::{postprocess, FrameAccumulator};

/// 流停止后多少毫秒内到达的散包按尾包丢弃，不触发隐式开流。
const STALE_AUDIO_WINDOW_MS: u64 = 300;

/// 会话要求上层（bridge）执行的动作。
#[derive(Debug, PartialEq)]
pub enum Action {
    /// 写入 TX 特征。
    SendTx(Vec<u8>),
    StreamStarted,
    Pcm(Vec<i16>),
    StreamStopped,
    /// 协议无法继续（如仅 8 kHz 编码），上层应断开并重连。
    Fatal(String),
}

pub struct AtvvSession {
    caps: Caps,
    caps_confirmed: bool,
    streaming: bool,
    mic_opened: bool,
    session_id: u8,
    last_stop_ms: Option<u64>,
    decoder: ImaAdpcmDecoder,
    frames: FrameAccumulator,
    pending_sync: Option<(i16, u8)>,
    gain_db: f64,
}

impl AtvvSession {
    pub fn new(gain_db: f64) -> Self {
        Self {
            caps: Caps::DEFAULT,
            caps_confirmed: false,
            streaming: false,
            mic_opened: false,
            session_id: 0,
            last_stop_ms: None,
            decoder: ImaAdpcmDecoder::new(),
            frames: FrameAccumulator::new(),
            pending_sync: None,
            gain_db,
        }
    }

    pub fn caps(&self) -> &Caps {
        &self.caps
    }

    pub fn is_streaming(&self) -> bool {
        self.streaming
    }

    /// 退出/断开时若麦克风仍开着，取出需要补发的 MIC_CLOSE 报文。
    pub fn take_mic_close(&mut self) -> Option<Vec<u8>> {
        if !self.mic_opened {
            return None;
        }
        self.mic_opened = false;
        Some(atvv::mic_close(self.caps.version, self.session_id))
    }

    pub fn handle_control(&mut self, data: &[u8], now_ms: u64, actions: &mut Vec<Action>) {
        let Some(event) = ControlEvent::parse(data) else {
            log::debug!("忽略无法解析的控制报文: {data:02X?}");
            return;
        };
        match event {
            ControlEvent::CapsResp => {
                let Some(caps) = Caps::parse(data) else {
                    actions.push(Action::Fatal("遥控器返回了无效的 ATVV 能力响应".into()));
                    return;
                };
                if caps.sample_rate != 16_000 {
                    actions.push(Action::Fatal(
                        "遥控器未提供受支持的 16 kHz 语音编码".into(),
                    ));
                    return;
                }
                log::info!(
                    "ATVV 能力: version={:#06X} codec={:#04X} frame={}B",
                    caps.version,
                    caps.codec,
                    caps.frame_size
                );
                self.caps = caps;
                self.caps_confirmed = true;
            }
            ControlEvent::StartSearch => {
                if !self.caps_confirmed {
                    log::warn!("能力协商未完成，忽略 START_SEARCH");
                    return;
                }
                log::info!("语音键按下，发送 MIC_OPEN");
                self.mic_opened = true;
                actions.push(Action::SendTx(atvv::mic_open(self.caps.version, self.caps.codec)));
            }
            ControlEvent::AudioStart { codec, session_id } => {
                if !self.caps_confirmed {
                    log::warn!("能力协商未完成，忽略 AUDIO_START");
                    return;
                }
                if let Some(codec) = codec {
                    self.caps = self.caps.with_stream_codec(codec);
                }
                if self.caps.sample_rate != 16_000 {
                    actions.push(Action::Fatal(
                        "遥控器切换到了不受支持的 8 kHz 语音编码".into(),
                    ));
                    return;
                }
                self.session_id = session_id.unwrap_or(0);
                self.start_streaming(actions);
            }
            ControlEvent::AudioStop => self.stop_streaming(now_ms, actions),
            ControlEvent::AudioSync { predictor, step_index } => {
                self.pending_sync = Some((predictor, step_index));
                self.frames.reset();
            }
            ControlEvent::Other(op) => log::debug!("忽略未知控制 opcode {op:#04X}"),
        }
    }

    pub fn handle_audio(&mut self, data: &[u8], now_ms: u64, actions: &mut Vec<Action>) {
        if !self.caps_confirmed {
            return;
        }
        if !self.streaming {
            // AUDIO_STOP 后的迟到散包直接丢弃；更早的音频则视为
            // AUDIO_START 尚未送达的竞态，隐式开流（真机观察到的行为）。
            if let Some(t) = self.last_stop_ms {
                if now_ms.saturating_sub(t) < STALE_AUDIO_WINDOW_MS {
                    return;
                }
            }
            log::debug!("音频先于 AUDIO_START 到达，隐式开流");
            self.start_streaming(actions);
        }

        let frame_size = self.caps.frame_size;
        let gain_db = self.gain_db;
        let mut pcm = Vec::new();
        let Self { frames, decoder, pending_sync, .. } = self;
        frames.push(data, frame_size, |frame| {
            if let Some((predictor, step_index)) = pending_sync.take() {
                decoder.reset(predictor as i32, step_index as i32);
            }
            let mut samples = Vec::with_capacity(frame.len() * 2);
            decoder.decode_into(frame, &mut samples);
            postprocess(&mut samples, gain_db);
            pcm.extend_from_slice(&samples);
        });
        if !pcm.is_empty() {
            actions.push(Action::Pcm(pcm));
        }
    }

    /// 会话被外部终止（断开、退出）时的收尾：补一个停流动作。
    pub fn finish(&mut self, actions: &mut Vec<Action>) {
        if self.streaming {
            self.streaming = false;
            actions.push(Action::StreamStopped);
        }
    }

    fn start_streaming(&mut self, actions: &mut Vec<Action>) {
        self.frames.reset();
        self.pending_sync = None;
        self.decoder.reset(0, 0);
        self.last_stop_ms = None;
        if !self.streaming {
            self.streaming = true;
            actions.push(Action::StreamStarted);
        }
    }

    fn stop_streaming(&mut self, now_ms: u64, actions: &mut Vec<Action>) {
        if !self.streaming {
            return;
        }
        self.streaming = false;
        self.frames.reset();
        self.pending_sync = None;
        self.last_stop_ms = Some(now_ms);
        actions.push(Action::StreamStopped);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// frame_size=1 的 16 kHz 能力响应，方便单字节音频就构成完整帧。
    const CAPS_FRAME1: [u8; 7] = [0x0B, 0x01, 0x00, 0x02, 0x03, 0x00, 0x01];

    fn confirmed_session() -> AtvvSession {
        let mut s = AtvvSession::new(0.0);
        let mut actions = Vec::new();
        s.handle_control(&CAPS_FRAME1, 0, &mut actions);
        assert!(actions.is_empty());
        s
    }

    #[test]
    fn start_search_triggers_mic_open() {
        let mut s = confirmed_session();
        let mut actions = Vec::new();
        s.handle_control(&[0x08], 0, &mut actions);
        assert_eq!(actions, vec![Action::SendTx(vec![0x0C, 0x00])]);
        assert_eq!(s.take_mic_close(), Some(vec![0x0D, 0x00]));
        assert_eq!(s.take_mic_close(), None);
    }

    #[test]
    fn start_search_before_caps_is_ignored() {
        let mut s = AtvvSession::new(0.0);
        let mut actions = Vec::new();
        s.handle_control(&[0x08], 0, &mut actions);
        assert!(actions.is_empty());
    }

    #[test]
    fn full_stream_flow_with_sync() {
        let mut s = confirmed_session();
        let mut actions = Vec::new();

        s.handle_control(&[0x04, 0x00, 0x02, 0x07], 10, &mut actions);
        assert_eq!(actions, vec![Action::StreamStarted]);
        assert!(s.is_streaming());
        actions.clear();

        // AUDIO_SYNC：predictor=100，step=5；随后单字节 0x00 构成一帧两样本。
        s.handle_control(&[0x0A, 0, 0, 0, 0x00, 0x64, 0x05], 20, &mut actions);
        s.handle_audio(&[0x00], 30, &mut actions);
        assert_eq!(actions, vec![Action::Pcm(vec![101, 102])]);
        actions.clear();

        s.handle_control(&[0x00], 40, &mut actions);
        assert_eq!(actions, vec![Action::StreamStopped]);
        assert!(!s.is_streaming());
        // MIC_CLOSE 携带 AUDIO_START 里的会话 ID。
        assert_eq!(s.take_mic_close(), None); // 没发过 START_SEARCH，麦克风不算开着
    }

    #[test]
    fn stale_audio_after_stop_is_dropped_then_implicit_start() {
        let mut s = confirmed_session();
        let mut actions = Vec::new();
        s.handle_control(&[0x04, 0x00, 0x02, 0x01], 0, &mut actions);
        s.handle_control(&[0x00], 1_000, &mut actions);
        actions.clear();

        // 停流后 300ms 内的散包：丢弃。
        s.handle_audio(&[0x00], 1_100, &mut actions);
        assert!(actions.is_empty());

        // 300ms 之后的音频：AUDIO_START 竞态，隐式开流。
        s.handle_audio(&[0x00], 1_400, &mut actions);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0], Action::StreamStarted);
        assert!(matches!(actions[1], Action::Pcm(_)));
    }

    #[test]
    fn audio_before_caps_is_ignored() {
        let mut s = AtvvSession::new(0.0);
        let mut actions = Vec::new();
        s.handle_audio(&[0x00], 0, &mut actions);
        assert!(actions.is_empty());
    }

    #[test]
    fn eight_khz_caps_is_fatal() {
        let mut s = AtvvSession::new(0.0);
        let mut actions = Vec::new();
        s.handle_control(&[0x0B, 0x01, 0x00, 0x01, 0x03, 0x00, 0x78], 0, &mut actions);
        assert!(matches!(actions.as_slice(), [Action::Fatal(_)]));
    }

    #[test]
    fn eight_khz_stream_switch_is_fatal() {
        let mut s = confirmed_session();
        let mut actions = Vec::new();
        s.handle_control(&[0x04, 0x00, 0x01, 0x02], 0, &mut actions);
        assert!(matches!(actions.as_slice(), [Action::Fatal(_)]));
    }

    #[test]
    fn frames_accumulate_across_notifications() {
        let mut s = AtvvSession::new(0.0);
        let mut actions = Vec::new();
        // frame_size=120 的默认能力。
        s.handle_control(&[0x0B, 0x01, 0x00, 0x02, 0x03, 0x00, 0x78], 0, &mut actions);
        s.handle_control(&[0x04, 0x00, 0x02, 0x01], 0, &mut actions);
        actions.clear();

        s.handle_audio(&[0u8; 100], 10, &mut actions);
        assert!(actions.is_empty()); // 未攒满 120 字节
        s.handle_audio(&[0u8; 100], 20, &mut actions);
        assert_eq!(actions.len(), 1); // 一帧 240 样本，剩 80 字节待攒
        match &actions[0] {
            Action::Pcm(samples) => assert_eq!(samples.len(), 240),
            other => panic!("期望 Pcm，得到 {other:?}"),
        }
    }

    #[test]
    fn finish_emits_stop_when_streaming() {
        let mut s = confirmed_session();
        let mut actions = Vec::new();
        s.handle_control(&[0x04, 0x00, 0x02, 0x01], 0, &mut actions);
        actions.clear();
        s.finish(&mut actions);
        assert_eq!(actions, vec![Action::StreamStopped]);
        s.finish(&mut actions);
        assert_eq!(actions.len(), 1);
    }
}
