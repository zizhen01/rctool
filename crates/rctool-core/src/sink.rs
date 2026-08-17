//! PCM 出口抽象。
//!
//! 会话解码出的 16 kHz 单声道 PCM 统一从 [`AudioSink`] 流出。v1 提供回环
//! 输出（见 [`crate::loopback`]）和 wav 落盘；将来 ASR、电平诊断、录音存档
//! 都是往这个 trait 后面加实现，核心链路不动。

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

/// 解码 PCM 的消费方。所有回调都在桥接任务上同步调用，实现方自己负责
/// 把重活挪去别的线程（回环实现即如此）。
pub trait AudioSink: Send {
    fn on_stream_start(&mut self) {}
    /// 16 kHz 单声道 16-bit PCM。
    fn push(&mut self, pcm: &[i16]);
    fn on_stream_stop(&mut self) {}
}

/// 丢弃一切（调试用）。
pub struct NullSink;

impl AudioSink for NullSink {
    fn push(&mut self, _pcm: &[i16]) {}
}

/// 扇出到多个 sink。
pub struct MultiSink {
    sinks: Vec<Box<dyn AudioSink>>,
}

impl MultiSink {
    pub fn new(sinks: Vec<Box<dyn AudioSink>>) -> Self {
        Self { sinks }
    }

    pub fn is_empty(&self) -> bool {
        self.sinks.is_empty()
    }
}

impl AudioSink for MultiSink {
    fn on_stream_start(&mut self) {
        for s in &mut self.sinks {
            s.on_stream_start();
        }
    }

    fn push(&mut self, pcm: &[i16]) {
        for s in &mut self.sinks {
            s.push(pcm);
        }
    }

    fn on_stream_stop(&mut self) {
        for s in &mut self.sinks {
            s.on_stream_stop();
        }
    }
}

/// 把全部会话的 PCM 顺序写进一个 16 kHz 单声道 wav（验证/调试用）。
pub struct WavSink {
    writer: Option<hound::WavWriter<BufWriter<File>>>,
    path: PathBuf,
    samples: u64,
}

impl WavSink {
    pub fn create(path: &Path) -> anyhow::Result<Self> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(path, spec)
            .map_err(|e| anyhow::anyhow!("无法创建 wav 文件 {}: {e}", path.display()))?;
        Ok(Self { writer: Some(writer), path: path.to_path_buf(), samples: 0 })
    }
}

impl AudioSink for WavSink {
    fn push(&mut self, pcm: &[i16]) {
        let Some(writer) = self.writer.as_mut() else { return };
        for &s in pcm {
            if let Err(e) = writer.write_sample(s) {
                log::error!("wav 写入失败，停止落盘: {e}");
                self.writer = None;
                return;
            }
        }
        self.samples += pcm.len() as u64;
    }

    fn on_stream_stop(&mut self) {
        if let Some(writer) = self.writer.as_mut() {
            let _ = writer.flush();
        }
    }
}

impl Drop for WavSink {
    fn drop(&mut self) {
        if let Some(writer) = self.writer.take() {
            match writer.finalize() {
                Ok(()) => log::info!(
                    "wav 已保存: {}（{} 样本，{:.1}s）",
                    self.path.display(),
                    self.samples,
                    self.samples as f64 / 16_000.0
                ),
                Err(e) => log::error!("wav 收尾失败: {e}"),
            }
        }
    }
}
