//! 应用状态。主线程独占，不加锁。

use crate::app::input::{InputBox, LineEnding};
use crate::app::record::Record;
use crate::mux::frame::Frame;
use crate::mux::session::DlcState;
use chrono::{DateTime, Local};
use std::collections::VecDeque;
use std::time::Duration;

/// 界面槽位数量。规格要求最多同时观察两个子通道。
pub const SLOT_COUNT: usize = 2;

#[derive(Debug, Clone)]
pub struct AppConfig {
    /// 每个通道保留的最大记录条数。
    pub max_records: usize,
    /// ASCII 模式下发送时自动追加的结束符。
    pub line_ending: LineEnding,
    /// 连续数据记录的合并窗口，零表示关闭合并。
    ///
    /// 有些模块（实测 Quectel EC800M）的 CMUX 实现逐字节成帧，一个 AT 响应
    /// 会拆成几十个 len=1 的 UIH 帧。不合并的话界面上一个响应刷掉几十行，
    /// 每行一个时间戳，根本没法读。
    pub merge_window: Duration,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            max_records: 5000,
            line_ending: LineEnding::default(),
            merge_window: Duration::from_millis(5),
        }
    }
}

/// 链路整体状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkState {
    /// 串口未打开。
    Down,
    /// 正在做 AT 握手。
    Negotiating,
    /// 复用已建立。
    Up,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub rx_frames: u64,
    pub tx_frames: u64,
    pub fcs_errors: u64,
    pub garbage_bytes: u64,
}

/// 原始帧视图的一条记录。
#[derive(Debug, Clone)]
pub struct RawEntry {
    pub ts: DateTime<Local>,
    /// true 表示本工具发出，false 表示收到。
    pub outgoing: bool,
    /// 已解析的摘要，如 `DLCI 1 UIH P/F=0 len=3`。
    pub summary: String,
    /// FCS 是否通过。解析失败的条目为 false。
    pub ok: bool,
    pub bytes: Vec<u8>,
}

impl RawEntry {
    /// 由一个已解析的帧构造条目。
    pub fn from_frame(f: &Frame, outgoing: bool) -> RawEntry {
        RawEntry {
            ts: Local::now(),
            outgoing,
            summary: format!(
                "DLCI {} {} P/F={} len={}",
                f.dlci,
                f.ftype.name(),
                if f.pf { 1 } else { 0 },
                f.info.len()
            ),
            ok: true,
            bytes: f.encode(),
        }
    }

    /// 由一段无法解析的字节构造条目。
    pub fn bad(summary: impl Into<String>, bytes: Vec<u8>) -> RawEntry {
        RawEntry {
            ts: Local::now(),
            outgoing: false,
            summary: summary.into(),
            ok: false,
            bytes,
        }
    }

    /// 视图中的一行文本。
    pub fn line(&self) -> String {
        format!(
            "{} {} {} {}",
            self.ts.format("%H:%M:%S%.3f"),
            if self.outgoing { "TX" } else { "RX" },
            self.summary,
            if self.ok { "" } else { "[FCS 失败]" }
        )
        .trim_end()
        .to_string()
    }
}

/// 当前打开的弹窗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dialog {
    /// 绑定 DLCI 到槽位。`input` 是正在输入的编号。
    Bind { slot: usize, input: String },
    /// 控制通道命令菜单。`selected` 是高亮项索引。
    Control { selected: usize },
    /// 快捷键一览。
    Help,
}

/// 快捷键帮助的内容：(按键, 说明)。
///
/// 与 `keymap::map_key` 的实现一一对应，改键位时两处要一起改——
/// `ui::dialog` 有测试盯着这张表不为空，但对不对得上只能靠人。
pub const HELP_KEYS: [(&str, &str); 13] = [
    ("F1 / F2 / Tab", "选择发送目标通道"),
    ("Enter", "发送"),
    ("↑ / ↓", "翻输入历史"),
    ("Ctrl+T", "切换 ASCII / HEX 输入模式"),
    ("Ctrl+O", "绑定 DLCI 到当前槽位并建链"),
    ("Ctrl+K", "控制通道命令 (MSC / DISC / CLD)"),
    ("Ctrl+R", "原始帧调试视图"),
    ("PgUp / PgDn", "滚动当前通道"),
    ("End", "回到跟随最新"),
    ("Ctrl+L", "清空当前通道"),
    ("Ctrl+S", "开始 / 停止记录日志"),
    ("Ctrl+G", "显示 / 关闭本帮助"),
    ("Ctrl+Q", "退出"),
];

/// 控制命令菜单项。
pub const CONTROL_ITEMS: [&str; 3] = [
    "发送 MSC（当前通道）",
    "关闭当前通道（DISC）",
    "关闭整个复用（CLD）",
];

/// 一个通道槽位。
#[derive(Debug)]
pub struct ChannelSlot {
    pub dlci: Option<u8>,
    pub state: DlcState,
    pub records: VecDeque<Record>,
    /// 是否自动跟随最新记录。
    pub follow: bool,
    /// 从底部往上偏移的记录条数。
    pub scroll: usize,
}

impl Default for ChannelSlot {
    fn default() -> Self {
        ChannelSlot {
            dlci: None,
            state: DlcState::Closed,
            records: VecDeque::new(),
            follow: true,
            scroll: 0,
        }
    }
}

pub struct AppState {
    pub cfg: AppConfig,
    pub slots: Vec<ChannelSlot>,
    /// 当前焦点槽位索引。
    pub focus: usize,
    pub input: InputBox,
    pub link: LinkState,
    pub stats: Stats,
    /// 端口描述，用于状态栏。
    pub port_desc: String,
    /// 落到未绑定 DLCI 上的记录数。
    pub unrouted: u64,
    /// 原始帧视图的环形日志。
    pub raw_log: VecDeque<RawEntry>,
    /// 是否正在显示原始帧视图。
    pub show_raw: bool,
    /// 当前弹窗，None 表示无。
    pub dialog: Option<Dialog>,
    /// 界面底部的一次性提示。
    pub notice: Option<String>,
}

impl AppState {
    pub fn new(cfg: AppConfig) -> AppState {
        AppState {
            cfg,
            slots: (0..SLOT_COUNT).map(|_| ChannelSlot::default()).collect(),
            focus: 0,
            input: InputBox::default(),
            link: LinkState::Down,
            stats: Stats::default(),
            port_desc: String::new(),
            unrouted: 0,
            raw_log: VecDeque::new(),
            show_raw: false,
            dialog: None,
            notice: None,
        }
    }

    /// 把 DLCI 绑定到空闲槽位，返回槽位索引。已绑定则返回原槽位。
    pub fn bind(&mut self, dlci: u8) -> Option<usize> {
        if let Some(i) = self.slot_of(dlci) {
            return Some(i);
        }
        let i = self.slots.iter().position(|s| s.dlci.is_none())?;
        self.slots[i] = ChannelSlot { dlci: Some(dlci), ..ChannelSlot::default() };
        Some(i)
    }

    /// 解绑 DLCI 并清空该槽位。
    pub fn unbind(&mut self, dlci: u8) {
        if let Some(i) = self.slot_of(dlci) {
            self.slots[i] = ChannelSlot::default();
        }
    }

    pub fn slot_of(&self, dlci: u8) -> Option<usize> {
        self.slots.iter().position(|s| s.dlci == Some(dlci))
    }

    /// 当前焦点槽位绑定的 DLCI。
    pub fn target_dlci(&self) -> Option<u8> {
        self.slots[self.focus].dlci
    }

    pub fn focus_next(&mut self) {
        self.focus = (self.focus + 1) % self.slots.len();
    }

    pub fn set_focus(&mut self, i: usize) {
        if i < self.slots.len() {
            self.focus = i;
        }
    }

    /// 更新某 DLCI 的链路状态。
    pub fn set_dlc_state(&mut self, dlci: u8, state: DlcState) {
        if let Some(i) = self.slot_of(dlci) {
            self.slots[i].state = state;
        }
    }

    /// 把记录投递到拥有该 DLCI 的槽位。
    pub fn push_record(&mut self, rec: Record) {
        let Some(i) = self.slot_of(rec.dlci) else {
            self.unrouted += 1;
            return;
        };
        let cap = self.cfg.max_records;
        let slot = &mut self.slots[i];
        slot.records.push_back(rec);
        while slot.records.len() > cap {
            slot.records.pop_front();
        }
        if slot.follow {
            slot.scroll = 0;
        }
    }

    /// 追加一条原始帧记录，超出容量时丢弃最旧的。
    pub fn push_raw(&mut self, entry: RawEntry) {
        self.raw_log.push_back(entry);
        while self.raw_log.len() > self.cfg.max_records {
            self.raw_log.pop_front();
        }
    }

    pub fn clear_focused(&mut self) {
        let slot = &mut self.slots[self.focus];
        slot.records.clear();
        slot.scroll = 0;
        slot.follow = true;
    }

    /// 滚动焦点槽位。负数向上（看更早的记录）。
    pub fn scroll_focused(&mut self, delta: isize) {
        let slot = &mut self.slots[self.focus];
        let max = slot.records.len();
        let new = (slot.scroll as isize - delta).clamp(0, max as isize) as usize;
        slot.scroll = new;
        slot.follow = new == 0;
    }

    /// 回到自动跟随。
    pub fn follow_focused(&mut self) {
        let slot = &mut self.slots[self.focus];
        slot.scroll = 0;
        slot.follow = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::record::{Direction, Record};

    #[test]
    fn starts_with_two_empty_slots() {
        let s = AppState::new(AppConfig::default());
        assert_eq!(s.slots.len(), 2);
        assert!(s.slots.iter().all(|sl| sl.dlci.is_none()));
        assert_eq!(s.focus, 0);
    }

    #[test]
    fn binding_a_dlci_occupies_a_slot() {
        let mut s = AppState::new(AppConfig::default());
        assert_eq!(s.bind(1), Some(0));
        assert_eq!(s.bind(3), Some(1));
        assert_eq!(s.bind(5), None, "只有两个槽位，第三个应被拒绝");
        assert_eq!(s.slots[0].dlci, Some(1));
        assert_eq!(s.slots[1].dlci, Some(3));
    }

    #[test]
    fn rebinding_same_dlci_returns_existing_slot() {
        let mut s = AppState::new(AppConfig::default());
        s.bind(1);
        assert_eq!(s.bind(1), Some(0));
    }

    #[test]
    fn unbinding_frees_the_slot() {
        let mut s = AppState::new(AppConfig::default());
        s.bind(1);
        s.unbind(1);
        assert!(s.slots[0].dlci.is_none());
        assert_eq!(s.bind(9), Some(0));
    }

    #[test]
    fn records_route_to_the_slot_owning_the_dlci() {
        let mut s = AppState::new(AppConfig::default());
        s.bind(1);
        s.bind(3);
        s.push_record(Record::new(Direction::Rx, 3, b"ok".to_vec()));
        assert_eq!(s.slots[0].records.len(), 0);
        assert_eq!(s.slots[1].records.len(), 1);
    }

    #[test]
    fn records_for_unbound_dlci_are_dropped_but_counted() {
        let mut s = AppState::new(AppConfig::default());
        s.push_record(Record::new(Direction::Rx, 7, b"x".to_vec()));
        assert!(s.slots.iter().all(|sl| sl.records.is_empty()));
        assert_eq!(s.unrouted, 1);
    }

    #[test]
    fn ring_buffer_evicts_oldest_beyond_capacity() {
        let cfg = AppConfig { max_records: 3, ..AppConfig::default() };
        let mut s = AppState::new(cfg);
        s.bind(1);
        for i in 0..5u8 {
            s.push_record(Record::new(Direction::Rx, 1, vec![i]));
        }
        assert_eq!(s.slots[0].records.len(), 3);
        assert_eq!(s.slots[0].records.front().unwrap().bytes, vec![2]);
    }

    #[test]
    fn focus_cycles_between_slots() {
        let mut s = AppState::new(AppConfig::default());
        s.focus_next();
        assert_eq!(s.focus, 1);
        s.focus_next();
        assert_eq!(s.focus, 0);
    }

    #[test]
    fn target_dlci_follows_focus() {
        let mut s = AppState::new(AppConfig::default());
        s.bind(1);
        s.bind(3);
        assert_eq!(s.target_dlci(), Some(1));
        s.focus_next();
        assert_eq!(s.target_dlci(), Some(3));
    }

    #[test]
    fn clearing_focused_slot_only_affects_that_slot() {
        let mut s = AppState::new(AppConfig::default());
        s.bind(1);
        s.bind(3);
        s.push_record(Record::new(Direction::Rx, 1, vec![1]));
        s.push_record(Record::new(Direction::Rx, 3, vec![3]));
        s.clear_focused();
        assert!(s.slots[0].records.is_empty());
        assert_eq!(s.slots[1].records.len(), 1);
    }

    #[test]
    fn scrolling_up_pins_the_view_and_end_resumes_follow() {
        let mut s = AppState::new(AppConfig::default());
        s.bind(1);
        for i in 0..10u8 {
            s.push_record(Record::new(Direction::Rx, 1, vec![i]));
        }
        assert!(s.slots[0].follow);
        s.scroll_focused(-3);
        assert!(!s.slots[0].follow);
        assert_eq!(s.slots[0].scroll, 3);
        s.follow_focused();
        assert!(s.slots[0].follow);
    }

    #[test]
    fn counters_track_traffic_and_errors() {
        let mut s = AppState::new(AppConfig::default());
        s.stats.rx_frames += 2;
        s.stats.fcs_errors += 1;
        assert_eq!(s.stats.rx_frames, 2);
        assert_eq!(s.stats.fcs_errors, 1);
    }

    #[test]
    fn raw_log_is_capped() {
        let cfg = AppConfig { max_records: 2, ..AppConfig::default() };
        let mut s = AppState::new(cfg);
        for i in 0..5 {
            s.push_raw(RawEntry::bad(format!("垃圾{i}"), vec![0xFF]));
        }
        assert_eq!(s.raw_log.len(), 2);
        assert!(s.raw_log.front().unwrap().summary.contains('3'));
    }

    #[test]
    fn raw_entry_from_frame_summarises_header() {
        let e = RawEntry::from_frame(&Frame::uih(1, b"abc".to_vec()), true);
        assert!(e.summary.contains("DLCI 1"), "实际: {}", e.summary);
        assert!(e.summary.contains("UIH"), "实际: {}", e.summary);
        assert!(e.summary.contains("len=3"), "实际: {}", e.summary);
        assert!(e.ok);
    }

    #[test]
    fn bad_raw_entry_is_marked_in_its_line() {
        let e = RawEntry::bad("FCS 不匹配", vec![0xF9, 0x03]);
        assert!(e.line().contains("[FCS 失败]"), "实际: {}", e.line());
    }
}
