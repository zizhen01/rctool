//! cpal 回环输出：把 16 kHz 单声道 PCM 送进用户选定的输出设备。
//!
//! 配合回环驱动（macOS 的 BlackHole、Windows 的 VB-Cable、Linux 的
//! PipeWire/Pulse null-sink），该设备的输入侧就成为系统里的"虚拟麦克风"，
//! 任何应用（系统听写、输入法语音）都能选它收音。
//!
//! 线程模型：cpal 的 `Stream` 在 macOS 上 `!Send`，因此由一个专用线程持有
//! 设备与流；桥接侧通过无锁环形缓冲把重采样后的样本推过去，音频回调只做
//! 出队和声道扇出，欠载时补零。

use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::sync::mpsc;
use std::thread::JoinHandle;

use crate::dsp::LinearResampler;
use crate::sink::AudioSink;

/// 已知回环驱动的名称特征，用于在设备列表里标注。
pub fn is_known_loopback(name: &str) -> bool {
    let lower = name.to_lowercase();
    ["blackhole", "vb-audio", "vb-cable", "cable input", "null output", "loopback"]
        .iter()
        .any(|k| lower.contains(k))
}

/// 枚举当前主机的输出设备名。
pub fn output_device_names() -> Vec<String> {
    let host = cpal::default_host();
    match host.output_devices() {
        Ok(devices) => devices.filter_map(|d| d.name().ok()).collect(),
        Err(_) => Vec::new(),
    }
}

/// 系统默认输出设备名（仅用于展示；本程序从不修改系统默认设备）。
pub fn default_output_name() -> Option<String> {
    cpal::default_host().default_output_device()?.name().ok()
}

pub struct LoopbackSink {
    producer: HeapProd<f32>,
    resampler: LinearResampler,
    scratch: Vec<f32>,
    device_name: String,
    sample_rate: u32,
    channels: u16,
    overflowed: bool,
    worker: Option<Worker>,
}

struct Worker {
    stop: mpsc::Sender<()>,
    join: JoinHandle<()>,
}

impl LoopbackSink {
    /// 按名称打开输出设备：先精确匹配，再大小写不敏感的子串匹配。
    pub fn open(name_query: &str) -> anyhow::Result<Self> {
        let host = cpal::default_host();
        let devices: Vec<cpal::Device> =
            host.output_devices().context("无法枚举音频输出设备")?.collect();
        let device = devices
            .iter()
            .find(|d| d.name().map(|n| n == name_query).unwrap_or(false))
            .or_else(|| {
                let q = name_query.to_lowercase();
                devices
                    .iter()
                    .find(|d| d.name().map(|n| n.to_lowercase().contains(&q)).unwrap_or(false))
            })
            .with_context(|| {
                format!(
                    "找不到输出设备 \"{name_query}\"。可用设备: {}",
                    output_device_names().join("、")
                )
            })?
            .clone();
        let device_name = device.name().unwrap_or_else(|_| name_query.to_string());

        let config = device
            .default_output_config()
            .with_context(|| format!("无法读取设备 \"{device_name}\" 的输出配置"))?;
        let sample_format = config.sample_format();
        let stream_config: cpal::StreamConfig = config.config();
        let sample_rate = stream_config.sample_rate.0;
        let channels = stream_config.channels;

        // 1 秒容量：语音由遥控器实时供给、回调实时消费，稳态占用极小；
        // 容量只是抖动余量，不构成累积延迟。
        let rb = HeapRb::<f32>::new(sample_rate as usize);
        let (producer, consumer) = rb.split();

        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let thread_name = device_name.clone();
        let join = std::thread::Builder::new()
            .name("rctool-audio".into())
            .spawn(move || {
                audio_thread(device, stream_config, sample_format, consumer, ready_tx, stop_rx);
            })
            .with_context(|| format!("无法启动音频线程（设备 \"{thread_name}\"）"))?;
        ready_rx
            .recv()
            .context("音频线程未报告状态")?
            .map_err(|e| anyhow::anyhow!("打开输出流失败（设备 \"{device_name}\"）: {e}"))?;

        log::info!("音频输出就绪: {device_name}（{sample_rate} Hz, {channels} 声道）");
        Ok(Self {
            producer,
            resampler: LinearResampler::new(16_000, sample_rate),
            scratch: Vec::new(),
            device_name,
            sample_rate,
            channels,
            overflowed: false,
            worker: Some(Worker { stop: stop_tx, join }),
        })
    }

    pub fn describe(&self) -> String {
        format!("{}（{} Hz, {} 声道）", self.device_name, self.sample_rate, self.channels)
    }
}

fn audio_thread(
    device: cpal::Device,
    config: cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    consumer: HeapCons<f32>,
    ready_tx: mpsc::Sender<Result<(), String>>,
    stop_rx: mpsc::Receiver<()>,
) {
    let channels = config.channels as usize;
    let built = match sample_format {
        cpal::SampleFormat::F32 => build_stream::<f32>(&device, &config, channels, consumer),
        cpal::SampleFormat::I16 => build_stream::<i16>(&device, &config, channels, consumer),
        cpal::SampleFormat::U16 => build_stream::<u16>(&device, &config, channels, consumer),
        other => Err(anyhow::anyhow!("不支持的采样格式 {other:?}")),
    };
    match built.and_then(|stream| {
        stream.play().context("启动输出流失败")?;
        Ok(stream)
    }) {
        Ok(stream) => {
            let _ = ready_tx.send(Ok(()));
            // 持有流直到收到停止信号（或对端整体销毁）。
            let _ = stop_rx.recv();
            drop(stream);
        }
        Err(e) => {
            let _ = ready_tx.send(Err(format!("{e:#}")));
        }
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    mut consumer: HeapCons<f32>,
) -> anyhow::Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            for frame in data.chunks_mut(channels) {
                let v = consumer.try_pop().unwrap_or(0.0);
                for sample in frame {
                    *sample = T::from_sample(v);
                }
            }
        },
        |e| log::warn!("音频输出流错误: {e}"),
        None,
    )?;
    Ok(stream)
}

impl AudioSink for LoopbackSink {
    fn on_stream_start(&mut self) {
        self.resampler.reset();
        self.overflowed = false;
    }

    fn push(&mut self, pcm: &[i16]) {
        self.scratch.clear();
        self.resampler.process(pcm, &mut self.scratch);
        let pushed = self.producer.push_slice(&self.scratch);
        if pushed < self.scratch.len() && !self.overflowed {
            self.overflowed = true;
            log::warn!(
                "输出缓冲溢出，丢弃 {} 样本（设备 \"{}\" 可能未在消费音频）",
                self.scratch.len() - pushed,
                self.device_name
            );
        }
    }
}

impl Drop for LoopbackSink {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.stop.send(());
            let _ = worker.join.join();
        }
    }
}
