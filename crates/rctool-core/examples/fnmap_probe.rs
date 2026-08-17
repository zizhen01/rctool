//! FFI 冒烟探针：创建 IOHID 客户端、枚举服务、匹配遥控器并试一轮
//! apply/restore。设备不在场时两者都应返回 0 且不崩溃。
//!
//! 运行：cargo run -p rctool-core --example fnmap_probe

fn main() {
    env_logger::Builder::new().filter_level(log::LevelFilter::Debug).init();
    let mut mapper = rctool_core::fnmap::VoiceKeyMapper::new();
    match mapper.apply() {
        Ok(n) => println!("apply: 匹配并写入 {n} 个 HID 服务"),
        Err(e) => println!("apply 失败: {e:#}"),
    }
    let restored = mapper.restore();
    println!("restore: 恢复 {restored} 个 HID 服务");
}
