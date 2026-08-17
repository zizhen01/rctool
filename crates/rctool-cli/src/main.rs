//! rctool：Xiaomi RC003 / ARN9 语音遥控器桥接 CLI。

use anyhow::Context;
use clap::{Parser, Subcommand};
use futures_lite::StreamExt;
use rctool_core::bridge::{self, BridgeOptions};
use rctool_core::loopback::{self, LoopbackSink};
use rctool_core::sink::{AudioSink, MultiSink, WavSink};
use rctool_core::{atvv, bluest::Adapter, tokio_util::sync::CancellationToken};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "rctool",
    version,
    about = "小米蓝牙语音遥控器（RC003 / ARN9）桌面桥接：ATVV 语音 → 虚拟麦克风"
)]
struct Cli {
    /// 输出调试日志
    #[arg(long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 列出音频输出设备（回环设备会标注，语音应输出到回环设备）
    Outputs,
    /// 查找遥控器：先查系统已连接设备，再扫广播
    Scan {
        /// 广播扫描时长（秒）
        #[arg(long, default_value_t = 10)]
        seconds: u64,
    },
    /// 运行桥接：按住遥控器麦克风键，语音进入指定出口
    Run {
        /// 输出设备名（支持子串匹配，如 "BlackHole"）
        #[arg(long)]
        output: Option<String>,
        /// 同时把 16 kHz PCM 写入 wav 文件（验证/调试用）
        #[arg(long)]
        wav: Option<PathBuf>,
        /// 数字增益 dB（-24 ~ 24）
        #[arg(long, default_value_t = 0.0, allow_negative_numbers = true)]
        gain: f64,
        /// 断线重连间隔（秒）
        #[arg(long, default_value_t = 3)]
        reconnect: u64,
        /// 关闭 F5→Fn/🌐 重映射（macOS；默认开启，按住麦克风键即触发系统听写）
        #[arg(long)]
        no_fn_remap: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    env_logger::Builder::new()
        .filter_level(if cli.verbose { log::LevelFilter::Debug } else { log::LevelFilter::Info })
        .format_timestamp_millis()
        .init();

    match cli.command {
        Command::Outputs => cmd_outputs(),
        Command::Scan { seconds } => cmd_scan(seconds).await,
        Command::Run { output, wav, gain, reconnect, no_fn_remap } => {
            cmd_run(output, wav, gain, reconnect, no_fn_remap).await
        }
    }
}

fn cmd_outputs() -> anyhow::Result<()> {
    let default_name = loopback::default_output_name();
    let names = loopback::output_device_names();
    anyhow::ensure!(!names.is_empty(), "没有枚举到任何音频输出设备");
    println!("音频输出设备：");
    let mut has_loopback = false;
    for name in &names {
        let mut tags = Vec::new();
        if Some(name) == default_name.as_ref() {
            tags.push("系统默认");
        }
        if loopback::is_known_loopback(name) {
            tags.push("回环设备 ← 建议语音输出到这里");
            has_loopback = true;
        }
        if tags.is_empty() {
            println!("  {name}");
        } else {
            println!("  {name}    [{}]", tags.join("，"));
        }
    }
    if !has_loopback {
        println!();
        println!("未发现回环设备。虚拟麦克风模式需要先安装一个：");
        println!("  macOS:   BlackHole 2ch  https://existential.audio/blackhole/");
        println!("  Windows: VB-Cable       https://vb-audio.com/Cable/");
        println!("  Linux:   pactl load-module module-null-sink sink_name=rctool");
    }
    Ok(())
}

async fn cmd_scan(seconds: u64) -> anyhow::Result<()> {
    let adapter = Adapter::default().await.context("没有可用的蓝牙适配器")?;
    adapter.wait_available().await.context("蓝牙适配器不可用")?;

    match adapter.connected_devices_with_services(&[atvv::SERVICE]).await {
        Ok(devices) if !devices.is_empty() => {
            println!("系统已连接、且带 ATVV 语音服务的设备：");
            for d in &devices {
                let name = d.name().unwrap_or_else(|_| "(无名称)".into());
                println!("  {name}  [{}]", d.id());
            }
            println!();
        }
        Ok(_) => println!("系统已连接设备中没有 ATVV 遥控器。\n"),
        Err(e) => println!("检索系统已连接设备失败: {e}\n"),
    }

    println!("广播扫描 {seconds}s（未配对的遥控器长按 主页+菜单 进入配对模式）…");
    let mut scan = adapter.scan(&[]).await.context("启动扫描失败")?;
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    let mut seen = HashSet::new();
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            _ = tokio::signal::ctrl_c() => break,
            item = scan.next() => {
                let Some(adv) = item else { break };
                if !seen.insert(adv.device.id()) {
                    continue;
                }
                let candidate = bridge::advertisement_is_candidate(&adv);
                let name = adv
                    .adv_data
                    .local_name
                    .clone()
                    .or_else(|| adv.device.name().ok())
                    .unwrap_or_else(|| "(无名称)".into());
                if candidate {
                    println!("  ★ {name}  rssi={:?}  ← 目标遥控器", adv.rssi);
                } else if cfg!(debug_assertions) {
                    println!("    {name}  rssi={:?}", adv.rssi);
                }
            }
        }
    }
    println!("扫描结束。");
    Ok(())
}

async fn cmd_run(
    output: Option<String>,
    wav: Option<PathBuf>,
    gain: f64,
    reconnect: u64,
    no_fn_remap: bool,
) -> anyhow::Result<()> {
    let mut sinks: Vec<Box<dyn AudioSink>> = Vec::new();
    if let Some(name) = &output {
        let sink = LoopbackSink::open(name)?;
        println!("语音输出 → {}", sink.describe());
        if !loopback::is_known_loopback(&sink.describe()) {
            log::warn!("所选设备不像回环设备：语音会直接从扬声器播出，而不会成为虚拟麦克风");
        }
        sinks.push(Box::new(sink));
    }
    if let Some(path) = &wav {
        sinks.push(Box::new(WavSink::create(path)?));
        println!("同时写入 wav → {}", path.display());
    }
    let mut sink = MultiSink::new(sinks);
    anyhow::ensure!(!sink.is_empty(), "至少指定 --output <设备名> 或 --wav <文件> 之一");

    let shutdown = CancellationToken::new();
    tokio::spawn({
        let shutdown = shutdown.clone();
        async move {
            let _ = tokio::signal::ctrl_c().await;
            log::info!("收到 Ctrl-C，正在退出…");
            shutdown.cancel();
        }
    });

    let opts = BridgeOptions {
        gain_db: gain,
        reconnect_delay: Duration::from_secs(reconnect.max(1)),
        fn_remap: !no_fn_remap,
        ..Default::default()
    };
    bridge::run(&mut sink, &opts, &shutdown).await
}
