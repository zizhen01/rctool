//! BLE 桥接主循环：发现 → 连接 → 订阅 → 事件泵 → 断线重连。
//!
//! 发现路径有讲究：macOS 上遥控器作为系统 HID 设备**已连接时不再广播**，
//! 所以主路径是"按服务检索系统已连接设备"（对应 CoreBluetooth 的
//! `retrieveConnectedPeripherals(withServices:)`，bluest 的
//! `connected_devices_with_services`）；广播扫描只是未配对/未连接时的回退。

use anyhow::Context as _;
use bluest::{Adapter, AdvertisingDevice, Characteristic, CharacteristicProperties, Device};
use futures_lite::StreamExt;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::atvv;
use crate::session::{Action, AtvvSession};
use crate::sink::AudioSink;

/// 小米遥控器已知的广播名（2 Pro / RC003 与普通款 / ARN9 同名同协议）。
pub const NAME_CANDIDATES: [&str; 3] =
    ["MI RC", "Xiaomi Bluetooth Remote 2 Pro", "小米蓝牙语音遥控器"];

pub fn name_matches(name: &str) -> bool {
    NAME_CANDIDATES.iter().any(|c| name == *c || name.starts_with(c))
}

#[derive(Debug, Clone)]
pub struct BridgeOptions {
    /// 数字增益（dB，±24 内有效）。
    pub gain_db: f64,
    /// 断开或失败后的重连间隔。
    pub reconnect_delay: Duration,
    /// 单轮广播扫描的超时。
    pub scan_timeout: Duration,
    /// macOS：连接后把遥控器的 F5（麦克风键）设备级重映射为 Fn/🌐，
    /// 配合系统「按住 🌐 开始听写」实现按住即听写；退出/断开自动恢复。
    pub fn_remap: bool,
}

impl Default for BridgeOptions {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            reconnect_delay: Duration::from_secs(3),
            scan_timeout: Duration::from_secs(15),
            fn_remap: true,
        }
    }
}

/// 运行桥接直到 `shutdown` 触发。内部自动断线重连。
pub async fn run(
    sink: &mut dyn AudioSink,
    opts: &BridgeOptions,
    shutdown: &CancellationToken,
) -> anyhow::Result<()> {
    let adapter = Adapter::default().await.context("没有可用的蓝牙适配器")?;
    adapter.wait_available().await.context("蓝牙适配器不可用")?;
    loop {
        if shutdown.is_cancelled() {
            return Ok(());
        }
        match connect_once(&adapter, sink, opts, shutdown).await {
            Ok(Ended::Shutdown) => return Ok(()),
            Ok(Ended::Disconnected) => {
                log::warn!("连接结束，{:.0}s 后重连", opts.reconnect_delay.as_secs_f64());
            }
            Err(e) => {
                log::warn!("桥接中断: {e:#}；{:.0}s 后重试", opts.reconnect_delay.as_secs_f64());
            }
        }
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = tokio::time::sleep(opts.reconnect_delay) => {}
        }
    }
}

enum Ended {
    Shutdown,
    Disconnected,
}

/// 广播包是否像目标遥控器：带 ATVV 服务，或名称匹配。
pub fn advertisement_is_candidate(adv: &AdvertisingDevice) -> bool {
    if adv.adv_data.services.contains(&atvv::SERVICE) {
        return true;
    }
    adv.adv_data.local_name.as_deref().map(name_matches).unwrap_or(false)
}

async fn find_device(
    adapter: &Adapter,
    opts: &BridgeOptions,
    shutdown: &CancellationToken,
) -> anyhow::Result<Option<Device>> {
    // 主路径：系统已连接、且暴露 ATVV 服务的设备（macOS 日常场景）。
    match adapter.connected_devices_with_services(&[atvv::SERVICE]).await {
        Ok(devices) => {
            if let Some(device) = devices.into_iter().next() {
                log::info!("在系统已连接设备中找到遥控器");
                return Ok(Some(device));
            }
        }
        Err(e) => log::debug!("检索系统已连接设备失败（继续扫描）: {e}"),
    }

    // 回退：广播扫描（遥控器未配对，或暂时断开正在广播）。
    log::info!("系统已连接设备中未找到，开始广播扫描（{:.0}s）…", opts.scan_timeout.as_secs_f64());
    let mut scan = adapter.scan(&[]).await.context("启动 BLE 扫描失败")?;
    let deadline = tokio::time::sleep(opts.scan_timeout);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(None),
            _ = &mut deadline => return Ok(None),
            item = scan.next() => match item {
                Some(adv) if advertisement_is_candidate(&adv) => {
                    log::info!(
                        "扫描到遥控器: {}",
                        adv.adv_data.local_name.as_deref().unwrap_or("(无名称)")
                    );
                    return Ok(Some(adv.device));
                }
                Some(_) => {}
                None => return Ok(None),
            },
        }
    }
}

async fn read_model_number(device: &Device) -> anyhow::Result<String> {
    let service = device
        .discover_services_with_uuid(atvv::DEVICE_INFORMATION_SERVICE)
        .await?
        .into_iter()
        .next()
        .context("无 Device Information 服务")?;
    let characteristic = service
        .discover_characteristics_with_uuid(atvv::MODEL_NUMBER_CHARACTERISTIC)
        .await?
        .into_iter()
        .next()
        .context("无型号特征")?;
    let raw = characteristic.read().await?;
    Ok(String::from_utf8_lossy(&raw).trim_matches('\0').trim().to_string())
}

async fn write_tx(
    tx: &Characteristic,
    props: Option<&CharacteristicProperties>,
    bytes: &[u8],
) -> bluest::Result<()> {
    let without_response = props.map(|p| p.write_without_response).unwrap_or(false);
    if without_response {
        tx.write_without_response(bytes).await
    } else {
        tx.write(bytes).await
    }
}

async fn connect_once(
    adapter: &Adapter,
    sink: &mut dyn AudioSink,
    opts: &BridgeOptions,
    shutdown: &CancellationToken,
) -> anyhow::Result<Ended> {
    let Some(device) = find_device(adapter, opts, shutdown).await? else {
        if shutdown.is_cancelled() {
            return Ok(Ended::Shutdown);
        }
        anyhow::bail!(
            "未发现遥控器。请确认已在系统蓝牙中配对，或长按遥控器 主页+菜单 键进入配对模式"
        );
    };

    let name = device.name().unwrap_or_else(|_| "未知设备".into());
    adapter.connect_device(&device).await.context("BLE 连接失败")?;
    log::info!("BLE 已连接: {name}");

    // 型号只用于日志与识别（RC003 = 2 Pro，ARN9 = 普通款），读不到不阻塞。
    match read_model_number(&device).await {
        Ok(model) if !model.is_empty() => log::info!("设备型号: {model}"),
        Ok(_) => {}
        Err(e) => log::debug!("读取型号失败（不影响语音）: {e:#}"),
    }

    let service = device
        .discover_services_with_uuid(atvv::SERVICE)
        .await
        .context("发现 ATVV 服务失败")?
        .into_iter()
        .next()
        .context("遥控器未提供 ATVV 语音服务")?;
    let characteristics =
        service.discover_characteristics().await.context("发现 ATVV 特征失败")?;
    let find = |uuid| characteristics.iter().find(|c| c.uuid() == uuid).cloned();
    let tx = find(atvv::CHAR_TX).context("缺少 ATVV TX 特征")?;
    let audio = find(atvv::CHAR_AUDIO).context("缺少 ATVV AUDIO 特征")?;
    let ctl = find(atvv::CHAR_CTL).context("缺少 ATVV CTL 特征")?;

    // 首次访问加密特征会触发系统配对流程；失败通常意味着尚未配对。
    let mut ctl_notifications = ctl
        .notify()
        .await
        .context("订阅控制通道失败（若从未配对，请先在系统蓝牙设置中配对遥控器）")?;
    let mut audio_notifications = audio.notify().await.context("订阅音频通道失败")?;

    // F5→Fn 重映射（仅 macOS）：设备此刻确定在场；断开/提前返回由
    // VoiceKeyMapper 的 Drop 兜底恢复，重连循环里每轮重新应用。
    let mut fn_mapper = (opts.fn_remap && cfg!(target_os = "macos"))
        .then(crate::fnmap::VoiceKeyMapper::new);
    if let Some(mapper) = fn_mapper.as_mut() {
        match mapper.apply() {
            Ok(0) => log::info!("未匹配到遥控器 HID 服务，本次跳过 F5→Fn 重映射"),
            Ok(n) => log::info!(
                "F5→Fn/🌐 已应用到 {n} 个 HID 服务；配合系统「按住 🌐 开始听写」即按住说话"
            ),
            Err(e) => log::warn!("F5→Fn 重映射失败（语音桥接不受影响）: {e:#}"),
        }
    }

    let tx_props = tx.properties().await.ok();
    let mut session = AtvvSession::new(opts.gain_db);
    write_tx(&tx, tx_props.as_ref(), &atvv::GET_CAPS).await.context("发送 GET_CAPS 失败")?;
    log::info!("ATVV 能力协商已发起，按住遥控器麦克风键即可说话");

    let epoch = Instant::now();
    let mut actions: Vec<Action> = Vec::new();

    let ended = loop {
        tokio::select! {
            _ = shutdown.cancelled() => break Ended::Shutdown,
            item = ctl_notifications.next() => match item {
                Some(Ok(data)) => {
                    session.handle_control(&data, epoch.elapsed().as_millis() as u64, &mut actions);
                }
                Some(Err(e)) => {
                    log::warn!("控制通道错误: {e}");
                    break Ended::Disconnected;
                }
                None => break Ended::Disconnected,
            },
            item = audio_notifications.next() => match item {
                Some(Ok(data)) => {
                    session.handle_audio(&data, epoch.elapsed().as_millis() as u64, &mut actions);
                }
                Some(Err(e)) => {
                    log::warn!("音频通道错误: {e}");
                    break Ended::Disconnected;
                }
                None => break Ended::Disconnected,
            },
        }

        let mut fatal: Option<String> = None;
        for action in actions.drain(..) {
            match action {
                Action::SendTx(bytes) => {
                    if let Err(e) = write_tx(&tx, tx_props.as_ref(), &bytes).await {
                        log::warn!("TX 写入失败: {e}");
                    }
                }
                Action::StreamStarted => {
                    log::info!("语音流开始");
                    sink.on_stream_start();
                }
                Action::Pcm(samples) => sink.push(&samples),
                Action::StreamStopped => {
                    log::info!("语音流结束");
                    sink.on_stream_stop();
                }
                Action::Fatal(reason) => fatal = Some(reason),
            }
        }
        if let Some(reason) = fatal {
            log::error!("协议错误: {reason}");
            break Ended::Disconnected;
        }
    };

    // 收尾：恢复 F5 映射、停流回调、补发 MIC_CLOSE、撤销本应用的连接兴趣。
    // （CoreBluetooth 语义：disconnect 只取消本进程的连接，系统 HID 链接不受影响。）
    if let Some(mapper) = fn_mapper.as_mut() {
        let restored = mapper.restore();
        if restored > 0 {
            log::info!("已恢复 F5 原映射（{restored} 个 HID 服务）");
        }
    }
    session.finish(&mut actions);
    for action in actions.drain(..) {
        if let Action::StreamStopped = action {
            log::info!("语音流结束（连接收尾）");
            sink.on_stream_stop();
        }
    }
    if let Some(close) = session.take_mic_close() {
        let _ = write_tx(&tx, tx_props.as_ref(), &close).await;
    }
    let _ = adapter.disconnect_device(&device).await;
    log::info!("BLE 已断开");
    Ok(ended)
}
