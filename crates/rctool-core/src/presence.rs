//! 遥控器在场检测——独立于语音桥接的轻量监视器。
//!
//! 为什么单独一层而不是复用 [`crate::bridge`]：桥接的存在意义是把语音接到回环
//! 设备上，没选输出设备它根本不启动。而"遥控器在不在"是个更基础的事实，防睡眠、
//! 自动解锁这类功能不该被迫先配好 BlackHole。所以在场检测自己持一个 adapter，
//! 只回答一个问题，谁都可以消费。
//!
//! 信号来源是**系统级连接状态**（CoreBluetooth 的
//! `retrieveConnectedPeripherals(withServices:)`），不是 RSSI。遥控器作为系统 HID
//! 设备连着的时候这条查询直接命中，既不用扫描也不用我们自己连上去——比按信号
//! 强度猜距离稳定得多，代价是分辨率只有"连着/没连着"。
//!
//! 抖动处理：查不到不立即判离开，要连续查不到 `absent_after` 才翻转。蓝牙 HID
//! 链路空闲时可能被系统短暂放开，按秒翻转会让防睡眠反复横跳。

use anyhow::Context as _;
use bluest::Adapter;
use futures_lite::StreamExt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::atvv;
use crate::bridge::{advertisement_is_candidate, name_matches};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Present,
    Absent,
}

/// 一台候选遥控器，供绑定界面列出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteInfo {
    /// `bluest::DeviceId` 的字符串形式。macOS 上是 CoreBluetooth 的 peripheral
    /// UUID——同一台 Mac 上对同一个遥控器稳定，换 Mac 会变。持久化存这个。
    pub id: String,
    pub name: String,
    /// 此刻是否已被系统连着（区分"就在手边"和"扫描到的邻居"）。
    pub connected: bool,
}

#[derive(Debug, Clone)]
pub struct PresenceOptions {
    /// 绑定的设备 id。`None` = 任意匹配的遥控器都算数（未绑定时的行为）。
    pub bound_id: Option<String>,
    /// 轮询间隔。
    pub poll_interval: Duration,
    /// 连续查不到多久才判为离开。
    pub absent_after: Duration,
}

impl Default for PresenceOptions {
    fn default() -> Self {
        Self {
            bound_id: None,
            poll_interval: Duration::from_secs(5),
            // 蓝牙 HID 链路空闲时可能被系统短暂放开，给足冗余再判离开。
            absent_after: Duration::from_secs(90),
        }
    }
}

pub type PresenceCallback = Arc<dyn Fn(Presence) + Send + Sync>;

/// 目标遥控器此刻是否在系统已连接设备里。
async fn seen_now(adapter: &Adapter, bound_id: Option<&str>) -> bool {
    let devices = match adapter.connected_devices_with_services(&[atvv::SERVICE]).await {
        Ok(devices) => devices,
        Err(e) => {
            log::debug!("检索系统已连接设备失败（本轮按未见处理）: {e}");
            return false;
        }
    };
    match bound_id {
        Some(bound) => devices.iter().any(|d| d.id().to_string() == bound),
        None => !devices.is_empty(),
    }
}

/// 持续监视在场状态直到 `shutdown` 触发。状态**翻转**时调用 `on_change`，
/// 并且启动后会先回调一次当前状态，调用方不必自己处理"初始值未知"。
pub async fn watch(
    opts: &PresenceOptions,
    shutdown: &CancellationToken,
    on_change: PresenceCallback,
) -> anyhow::Result<()> {
    let adapter = Adapter::default().await.context("没有可用的蓝牙适配器")?;
    adapter.wait_available().await.context("蓝牙适配器不可用")?;

    let bound = opts.bound_id.as_deref();
    let mut reported: Option<Presence> = None;
    // 首次查不到时不必等满 absent_after——启动时的"没有"就是"没有"。
    let mut missing_since: Option<Instant> = Some(Instant::now() - opts.absent_after);

    loop {
        let now = Instant::now();
        let current = if seen_now(&adapter, bound).await {
            missing_since = None;
            Presence::Present
        } else {
            let since = *missing_since.get_or_insert(now);
            if now.duration_since(since) >= opts.absent_after {
                Presence::Absent
            } else {
                // 宽限期内维持原判；没有原判就按不在场。
                reported.unwrap_or(Presence::Absent)
            }
        };

        if reported != Some(current) {
            log::info!("遥控器在场状态: {current:?}");
            reported = Some(current);
            on_change(current);
        }

        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = tokio::time::sleep(opts.poll_interval) => {}
        }
    }
}

/// 列出可绑定的遥控器：系统已连接的 + 扫描到在广播的，按 id 去重。
///
/// 已连接的排在前面——那基本就是用户手上这台。
pub async fn list_remotes(scan_timeout: Duration) -> anyhow::Result<Vec<RemoteInfo>> {
    let adapter = Adapter::default().await.context("没有可用的蓝牙适配器")?;
    adapter.wait_available().await.context("蓝牙适配器不可用")?;

    let mut found: Vec<RemoteInfo> = Vec::new();
    let mut push = |info: RemoteInfo| {
        if !found.iter().any(|f| f.id == info.id) {
            found.push(info);
        }
    };

    match adapter.connected_devices_with_services(&[atvv::SERVICE]).await {
        Ok(devices) => {
            for device in devices {
                push(RemoteInfo {
                    id: device.id().to_string(),
                    name: device.name().unwrap_or_else(|_| "未知设备".into()),
                    connected: true,
                });
            }
        }
        Err(e) => log::debug!("检索系统已连接设备失败（继续扫描）: {e}"),
    }

    // 未配对/未连接的遥控器只能靠广播发现。扫到就够，不连接、不读型号——
    // 连接会触发系统配对流程，那是用户在系统蓝牙设置里该做的事。
    let mut scan = adapter.scan(&[]).await.context("启动 BLE 扫描失败")?;
    let deadline = tokio::time::sleep(scan_timeout);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            item = scan.next() => match item {
                Some(adv) if advertisement_is_candidate(&adv) => {
                    let name = adv
                        .adv_data
                        .local_name
                        .clone()
                        .unwrap_or_else(|| "未知设备".into());
                    push(RemoteInfo { id: adv.device.id().to_string(), name, connected: false });
                }
                Some(_) => {}
                None => break,
            },
        }
    }

    // 名称能对上已知型号的排前面，剩下的（靠 ATVV 服务命中的）压后。
    found.sort_by_key(|r| (!r.connected, !name_matches(&r.name)));
    Ok(found)
}
