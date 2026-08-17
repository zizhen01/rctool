//! 遥控器按键 → 主机动作的映射模型（跨平台纯数据）。
//!
//! 这一层不做 I/O，只定义"哪个键、默认干什么、能改成什么"，同时驱动设置
//! 界面。macOS 的实际读取与注入在 [`crate::hidmap`]。
//!
//! 设计要点：**恒等映射自动直通**。遥控器多数键（方向、OK、音量）配对后
//! 系统原生行为就是对的；只有当某个键被映射成与原生不同的动作时，才需要
//! 拦截原生事件并注入新动作。这样默认只有 电源 / 返回 / TV / 主页 / 菜单
//! 五个键进入拦截路径，其余保持零开销原生直通。

/// 遥控器实体键。语音键（麦克风）不在此列——它由 [`crate::fnmap`] 单独
/// 处理（F5→Fn 触发系统听写），不参与通用动作映射。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RemoteButton {
    Power,
    Up,
    Down,
    Left,
    Right,
    Ok,
    Back,
    Home,
    Menu,
    Tv,
    VolumeUp,
    VolumeDown,
}

impl RemoteButton {
    pub const ALL: [RemoteButton; 12] = [
        RemoteButton::Power,
        RemoteButton::Up,
        RemoteButton::Down,
        RemoteButton::Left,
        RemoteButton::Right,
        RemoteButton::Ok,
        RemoteButton::Back,
        RemoteButton::Home,
        RemoteButton::Menu,
        RemoteButton::Tv,
        RemoteButton::VolumeUp,
        RemoteButton::VolumeDown,
    ];

    /// HID 键盘页 usage（遥控器在 input report 里上报的值）。
    pub fn hid_usage(self) -> u16 {
        match self {
            RemoteButton::Power => 0x66,
            RemoteButton::Up => 0x52,
            RemoteButton::Down => 0x51,
            RemoteButton::Left => 0x50,
            RemoteButton::Right => 0x4F,
            RemoteButton::Ok => 0x28,
            RemoteButton::Back => 0xF1,
            RemoteButton::Home => 0x4A,
            RemoteButton::Menu => 0x65,
            RemoteButton::Tv => 0x35,
            RemoteButton::VolumeUp => 0x80,
            RemoteButton::VolumeDown => 0x81,
        }
    }

    /// 稳定字符串 ID（配置持久化、前端通信用）。
    pub fn id(self) -> &'static str {
        match self {
            RemoteButton::Power => "power",
            RemoteButton::Up => "up",
            RemoteButton::Down => "down",
            RemoteButton::Left => "left",
            RemoteButton::Right => "right",
            RemoteButton::Ok => "ok",
            RemoteButton::Back => "back",
            RemoteButton::Home => "home",
            RemoteButton::Menu => "menu",
            RemoteButton::Tv => "tv",
            RemoteButton::VolumeUp => "volume_up",
            RemoteButton::VolumeDown => "volume_down",
        }
    }

    pub fn from_id(id: &str) -> Option<RemoteButton> {
        RemoteButton::ALL.into_iter().find(|b| b.id() == id)
    }

    /// 反查：HID usage → 按钮。麦克风键 usage(0x3E) 不属于任何 RemoteButton，
    /// 返回 None（它由 fnmap 处理，读取器应忽略）。
    pub fn from_hid_usage(usage: u16) -> Option<RemoteButton> {
        RemoteButton::ALL.into_iter().find(|b| b.hid_usage() == usage)
    }

    pub fn label(self) -> &'static str {
        match self {
            RemoteButton::Power => "电源",
            RemoteButton::Up => "上",
            RemoteButton::Down => "下",
            RemoteButton::Left => "左",
            RemoteButton::Right => "右",
            RemoteButton::Ok => "确定",
            RemoteButton::Back => "返回",
            RemoteButton::Home => "主页",
            RemoteButton::Menu => "菜单",
            RemoteButton::Tv => "TV",
            RemoteButton::VolumeUp => "音量 +",
            RemoteButton::VolumeDown => "音量 −",
        }
    }

    /// 配对后系统对这个键的**原生**处理（不干预时的行为）。用于恒等直通
    /// 判定：若映射动作解析后与此相同，则无需拦截+注入。
    ///
    /// `None` 表示系统对该键没有可用原生行为——例如返回键 0xF1 是系统层
    /// 死键（直接丢弃），电源/音量是 systemDefined（本模型不作为键盘事件
    /// 处理，音量默认保持原生）。
    pub fn native(self) -> Option<Injection> {
        let key = |keycode| Some(Injection { keycode, mods: Mods::NONE });
        match self {
            RemoteButton::Up => key(126),
            RemoteButton::Down => key(125),
            RemoteButton::Left => key(123),
            RemoteButton::Right => key(124),
            RemoteButton::Ok => key(36),
            RemoteButton::Tv => key(50),   // 原生打出 ` 反引号
            RemoteButton::Home => key(115), // 原生 Home
            RemoteButton::Menu => key(110), // 原生 Application 键
            // 返回：系统丢弃；电源/音量：systemDefined，不在键盘模型内。
            RemoteButton::Back | RemoteButton::Power => None,
            RemoteButton::VolumeUp | RemoteButton::VolumeDown => None,
        }
    }
}

/// 修饰键位集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Mods(pub u8);

impl Mods {
    pub const NONE: Mods = Mods(0);
    pub const COMMAND: Mods = Mods(1 << 0);
    pub const SHIFT: Mods = Mods(1 << 1);
    pub const CONTROL: Mods = Mods(1 << 2);
    pub const OPTION: Mods = Mods(1 << 3);
    pub const FN: Mods = Mods(1 << 4);

    pub fn contains(self, other: Mods) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn union(self, other: Mods) -> Mods {
        Mods(self.0 | other.0)
    }
}

/// 一次键盘注入：虚拟键码 + 修饰键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Injection {
    pub keycode: u16,
    pub mods: Mods,
}

/// 可分配给按键的动作。v1 全部解析为键盘事件（不含 systemDefined 音量/媒体，
/// 音量键默认保持原生直通）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 保持系统原生行为（不拦截、不注入）。
    Native,
    /// 禁用：拦截原生行为但什么都不发。
    Disabled,
    Escape,
    Return,
    Tab,
    Space,
    Delete,
    ForwardDelete,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    /// 调度中心（Control + ↑）。
    MissionControl,
    /// 切换应用（Command + Tab）。
    AppSwitcher,
    /// 上下文菜单（Shift + F10）。
    ContextMenu,
    /// 显示桌面（Fn + F11）。
    ShowDesktop,
}

impl Action {
    /// 前端下拉可选项（Native/Disabled 之外的具体动作）。
    pub const ASSIGNABLE: [Action; 18] = [
        Action::Escape,
        Action::Return,
        Action::Tab,
        Action::Space,
        Action::Delete,
        Action::ForwardDelete,
        Action::ArrowUp,
        Action::ArrowDown,
        Action::ArrowLeft,
        Action::ArrowRight,
        Action::Home,
        Action::End,
        Action::PageUp,
        Action::PageDown,
        Action::MissionControl,
        Action::AppSwitcher,
        Action::ContextMenu,
        Action::ShowDesktop,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Action::Native => "native",
            Action::Disabled => "disabled",
            Action::Escape => "escape",
            Action::Return => "return",
            Action::Tab => "tab",
            Action::Space => "space",
            Action::Delete => "delete",
            Action::ForwardDelete => "forward_delete",
            Action::ArrowUp => "arrow_up",
            Action::ArrowDown => "arrow_down",
            Action::ArrowLeft => "arrow_left",
            Action::ArrowRight => "arrow_right",
            Action::Home => "home",
            Action::End => "end",
            Action::PageUp => "page_up",
            Action::PageDown => "page_down",
            Action::MissionControl => "mission_control",
            Action::AppSwitcher => "app_switcher",
            Action::ContextMenu => "context_menu",
            Action::ShowDesktop => "show_desktop",
        }
    }

    pub fn from_id(id: &str) -> Option<Action> {
        let all = [Action::Native, Action::Disabled]
            .into_iter()
            .chain(Action::ASSIGNABLE);
        all.into_iter().find(|a| a.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::Native => "保持原生",
            Action::Disabled => "禁用",
            Action::Escape => "Escape",
            Action::Return => "Return",
            Action::Tab => "Tab",
            Action::Space => "空格",
            Action::Delete => "Delete（退格）",
            Action::ForwardDelete => "向前删除",
            Action::ArrowUp => "方向上",
            Action::ArrowDown => "方向下",
            Action::ArrowLeft => "方向左",
            Action::ArrowRight => "方向右",
            Action::Home => "Home",
            Action::End => "End",
            Action::PageUp => "Page Up",
            Action::PageDown => "Page Down",
            Action::MissionControl => "调度中心",
            Action::AppSwitcher => "切换应用",
            Action::ContextMenu => "上下文菜单",
            Action::ShowDesktop => "显示桌面",
        }
    }

    /// 解析为键盘注入。`Native`/`Disabled` 无注入内容。
    pub fn injection(self) -> Option<Injection> {
        let key = |keycode, mods| Some(Injection { keycode, mods });
        match self {
            Action::Native | Action::Disabled => None,
            Action::Escape => key(53, Mods::NONE),
            Action::Return => key(36, Mods::NONE),
            Action::Tab => key(48, Mods::NONE),
            Action::Space => key(49, Mods::NONE),
            Action::Delete => key(51, Mods::NONE),
            Action::ForwardDelete => key(117, Mods::NONE),
            Action::ArrowUp => key(126, Mods::NONE),
            Action::ArrowDown => key(125, Mods::NONE),
            Action::ArrowLeft => key(123, Mods::NONE),
            Action::ArrowRight => key(124, Mods::NONE),
            Action::Home => key(115, Mods::NONE),
            Action::End => key(119, Mods::NONE),
            Action::PageUp => key(116, Mods::NONE),
            Action::PageDown => key(121, Mods::NONE),
            Action::MissionControl => key(126, Mods::CONTROL),
            Action::AppSwitcher => key(48, Mods::COMMAND),
            Action::ContextMenu => key(109, Mods::SHIFT), // F10
            Action::ShowDesktop => key(103, Mods::FN),    // F11
        }
    }
}

/// 单个按键的运行时处置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// 放行系统原生行为，读取器不干预此键。
    Passthrough,
    /// 拦截原生行为但不注入（禁用）。
    Suppress,
    /// 拦截原生行为并注入指定键（若原生为空则无需拦截，仅注入）。
    Remap(Injection),
}

/// 完整按键映射表。
#[derive(Debug, Clone)]
pub struct KeyMap {
    actions: std::collections::HashMap<RemoteButton, Action>,
}

impl Default for KeyMap {
    fn default() -> Self {
        Self::with_defaults()
    }
}

impl KeyMap {
    /// 出厂默认：规避 TV 反引号 / 电源关机对话框，救活返回键，其余合理化。
    pub fn with_defaults() -> Self {
        use Action as A;
        use RemoteButton as B;
        let pairs = [
            (B::Power, A::Escape),      // 原生会弹关机对话框
            (B::Up, A::ArrowUp),
            (B::Down, A::ArrowDown),
            (B::Left, A::ArrowLeft),
            (B::Right, A::ArrowRight),
            (B::Ok, A::Return),
            (B::Back, A::Delete),       // 原生是系统层死键
            (B::Home, A::MissionControl),
            (B::Menu, A::ContextMenu),
            (B::Tv, A::AppSwitcher),    // 原生会打出反引号
            (B::VolumeUp, A::Native),
            (B::VolumeDown, A::Native),
        ];
        Self { actions: pairs.into_iter().collect() }
    }

    pub fn action(&self, button: RemoteButton) -> Action {
        self.actions.get(&button).copied().unwrap_or(Action::Native)
    }

    pub fn set(&mut self, button: RemoteButton, action: Action) {
        self.actions.insert(button, action);
    }

    /// 计算某键的运行时处置：把恒等映射折叠为直通，避免无谓拦截。
    pub fn disposition(&self, button: RemoteButton) -> Disposition {
        match self.action(button) {
            Action::Native => Disposition::Passthrough,
            Action::Disabled => match button.native() {
                // 原生本就无行为（返回/电源/音量）→ 禁用即直通。
                None => Disposition::Passthrough,
                Some(_) => Disposition::Suppress,
            },
            action => {
                let injection = action.injection().expect("具体动作必有注入");
                // 映射结果恰等于原生行为 → 直通（零开销）。
                if button.native() == Some(injection) {
                    Disposition::Passthrough
                } else {
                    Disposition::Remap(injection)
                }
            }
        }
    }

    /// 需要读取器干预（拦截或注入）的键——即非直通的键。默认只有
    /// 电源/返回/TV/主页/菜单。
    pub fn managed_buttons(&self) -> Vec<RemoteButton> {
        RemoteButton::ALL
            .into_iter()
            .filter(|&b| self.disposition(b) != Disposition::Passthrough)
            .collect()
    }
}

/// 从遥控器 HID input report 解析出当前按下的键盘页 usage 集合。
///
/// 报文格式（真机实测）：reportID 1；若长度 7 且首字节等于 reportID 则先剥离，
/// 其后按小端 u16 成对排列，非零即为一个正在按下的 usage。
pub fn parse_report_usages(report_id: u32, data: &[u8]) -> Option<Vec<u16>> {
    if report_id != 1 {
        return None;
    }
    let mut bytes = data;
    if bytes.len() == 7 && bytes.first() == Some(&(report_id as u8)) {
        bytes = &bytes[1..];
    }
    if bytes.is_empty() || !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::new();
    for pair in bytes.chunks_exact(2) {
        let usage = u16::from_le_bytes([pair[0], pair[1]]);
        if usage != 0 {
            out.push(usage);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_parser_strips_report_id_and_reads_pairs() {
        // reportID 前缀 + OK(0x28) + 方向下(0x51)。
        let usages = parse_report_usages(1, &[0x01, 0x28, 0x00, 0x51, 0x00, 0x00, 0x00]).unwrap();
        assert!(usages.contains(&0x28) && usages.contains(&0x51));
        // 无前缀的偶数长度报文。
        let usages = parse_report_usages(1, &[0xF1, 0x00]).unwrap();
        assert_eq!(usages, vec![0xF1]);
        // 非 1 号报文忽略；奇数长度拒绝。
        assert_eq!(parse_report_usages(2, &[0x28, 0x00]), None);
        assert_eq!(parse_report_usages(1, &[0x28, 0x00, 0x51]), None);
    }

    #[test]
    fn defaults_fold_identity_mappings_to_passthrough() {
        let map = KeyMap::with_defaults();
        // 方向键 / OK / 音量：映射等于原生（或 Native）→ 直通。
        for b in [
            RemoteButton::Up,
            RemoteButton::Down,
            RemoteButton::Left,
            RemoteButton::Right,
            RemoteButton::Ok,
            RemoteButton::VolumeUp,
            RemoteButton::VolumeDown,
        ] {
            assert_eq!(map.disposition(b), Disposition::Passthrough, "{}", b.label());
        }
    }

    #[test]
    fn defaults_manage_exactly_the_five_problem_keys() {
        let map = KeyMap::with_defaults();
        let managed = map.managed_buttons();
        assert_eq!(managed.len(), 5);
        for b in [
            RemoteButton::Power,
            RemoteButton::Back,
            RemoteButton::Tv,
            RemoteButton::Home,
            RemoteButton::Menu,
        ] {
            assert!(managed.contains(&b), "{} 应被管理", b.label());
        }
    }

    #[test]
    fn back_key_remaps_without_native_to_suppress() {
        // 返回键原生为空：Remap 只注入 Delete，拦截侧无需抵消。
        let map = KeyMap::with_defaults();
        assert_eq!(
            map.disposition(RemoteButton::Back),
            Disposition::Remap(Injection { keycode: 51, mods: Mods::NONE })
        );
        assert_eq!(RemoteButton::Back.native(), None);
    }

    #[test]
    fn tv_remap_carries_appswitcher_injection() {
        let map = KeyMap::with_defaults();
        assert_eq!(
            map.disposition(RemoteButton::Tv),
            Disposition::Remap(Injection { keycode: 48, mods: Mods::COMMAND })
        );
    }

    #[test]
    fn setting_arrow_to_its_native_is_passthrough() {
        let mut map = KeyMap::with_defaults();
        map.set(RemoteButton::Up, Action::ArrowUp);
        assert_eq!(map.disposition(RemoteButton::Up), Disposition::Passthrough);
        map.set(RemoteButton::Up, Action::Escape);
        assert_eq!(
            map.disposition(RemoteButton::Up),
            Disposition::Remap(Injection { keycode: 53, mods: Mods::NONE })
        );
    }

    #[test]
    fn disabling_a_native_key_suppresses_but_deadkey_passes() {
        let mut map = KeyMap::with_defaults();
        map.set(RemoteButton::Tv, Action::Disabled);
        assert_eq!(map.disposition(RemoteButton::Tv), Disposition::Suppress);
        // 返回键无原生行为，禁用等于直通。
        map.set(RemoteButton::Back, Action::Disabled);
        assert_eq!(map.disposition(RemoteButton::Back), Disposition::Passthrough);
    }

    #[test]
    fn action_and_button_id_roundtrip() {
        for a in Action::ASSIGNABLE {
            assert_eq!(Action::from_id(a.id()), Some(a));
        }
        for b in RemoteButton::ALL {
            assert_eq!(RemoteButton::from_id(b.id()), Some(b));
        }
    }
}
