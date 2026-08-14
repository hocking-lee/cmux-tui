//! 运行时：唯一同时持有界面状态与协议状态机的地方。
//!
//! 事件从这里进入状态机，状态机产出的动作在这里变成实际发送与界面更新。
//! 它只往写线程的 channel 投递，不直接接触串口，因此完全可测。

use crate::app::event::{Event, TxRequest};
use crate::app::keymap::Command;
use crate::app::log::SessionLog;
use crate::app::record::{Direction, Record};
use crate::app::state::{AppConfig, AppState, LinkState, RawEntry};
use crate::mux::control::ControlMessage;
use crate::mux::frame::Frame;
use crate::mux::session::{Action, DlcState, Session, SessionConfig};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Instant;

/// MSC 中表示 DV+RTC+RTR 置位的常用信号值。
const MSC_SIGNALS_READY: u8 = 0x8D;

/// 正在累积、尚未落到界面与日志的连续数据。
///
/// 存在的理由：部分模块（实测 Quectel EC800M）逐字节成帧，一个 AT 响应会被
/// 拆成几十个 `len=1` 的 UIH 帧。按帧粒度显示的话一个响应刷掉几十行、每行一个
/// 时间戳，完全没法读。这里把窗口内连续到达的同向同通道数据并成一条。
/// 帧边界信息不会丢——原始帧视图仍然逐帧记录。
struct Pending {
    dlci: u8,
    dir: Direction,
    /// 首帧到达的时刻，作为合并后记录的时间戳。
    ts: chrono::DateTime<chrono::Local>,
    bytes: Vec<u8>,
    /// 最后一帧到达的单调时刻，用来判断是否还在窗口内。
    last: Instant,
}

pub struct Runtime {
    pub state: AppState,
    pub session: Session,
    pub tx: Sender<TxRequest>,
    pub log: Option<SessionLog>,
    pub log_path: Option<PathBuf>,
    pub running: bool,
    pending: Option<Pending>,
}

impl Runtime {
    pub fn new(app_cfg: AppConfig, sess_cfg: SessionConfig, tx: Sender<TxRequest>) -> Runtime {
        Runtime {
            state: AppState::new(app_cfg),
            session: Session::new(sess_cfg),
            tx,
            log: None,
            log_path: None,
            running: true,
            pending: None,
        }
    }

    /// 把一条记录真正投递出去：写日志 + 落到界面。
    fn emit(&mut self, rec: Record) {
        if let Some(l) = self.log.as_mut() {
            let _ = l.write_record(&rec);
        }
        self.state.push_record(rec);
    }

    /// 把正在累积的数据冲刷出去。
    fn flush_pending(&mut self) {
        if let Some(p) = self.pending.take() {
            self.emit(Record {
                ts: p.ts,
                dir: p.dir,
                dlci: p.dlci,
                bytes: p.bytes,
                note: None,
            });
        }
    }

    /// 累积超过合并窗口时冲刷。由 tick 周期性驱动。
    fn flush_if_stale(&mut self, now: Instant) {
        let window = self.state.cfg.merge_window;
        if let Some(p) = &self.pending
            && now.duration_since(p.last) > window
        {
            self.flush_pending();
        }
    }

    /// 记录一条内容。数据记录会在合并窗口内累积，提示与错误则立即落地。
    fn record(&mut self, rec: Record, now: Instant) {
        // 提示/错误不参与合并，但要先冲刷已累积的数据，否则时序会乱。
        if rec.note.is_some() {
            self.flush_pending();
            self.emit(rec);
            return;
        }

        let window = self.state.cfg.merge_window;
        if window.is_zero() {
            self.emit(rec);
            return;
        }

        let mergeable = self.pending.as_ref().is_some_and(|p| {
            p.dlci == rec.dlci && p.dir == rec.dir && now.duration_since(p.last) <= window
        });

        if mergeable {
            let p = self.pending.as_mut().expect("已判定可合并");
            p.bytes.extend_from_slice(&rec.bytes);
            p.last = now;
        } else {
            self.flush_pending();
            self.pending = Some(Pending {
                dlci: rec.dlci,
                dir: rec.dir,
                ts: rec.ts,
                bytes: rec.bytes,
                last: now,
            });
        }
    }

    /// 把一帧投递给写线程，并记入原始帧日志与计数。
    fn send_frame(&mut self, f: Frame) {
        self.state.push_raw(RawEntry::from_frame(&f, true));
        self.state.stats.tx_frames += 1;
        let _ = self.tx.send(TxRequest::Frame(f));
    }

    /// 执行状态机产出的动作。
    fn apply(&mut self, actions: Vec<Action>, now: Instant) {
        for act in actions {
            match act {
                Action::Send(f) => self.send_frame(f),
                Action::Opened(dlci) => {
                    self.state.set_dlc_state(dlci, DlcState::Open);
                    // 对端在复用协议上回了 UA，链路显然是通的。这会纠正握手阶段
                    // 的误判：模块若已处于 CMUX 模式就不认裸 AT，握手失败并不
                    // 代表复用不可用，此时状态栏不该继续显示 MUX DOWN。
                    self.state.link = LinkState::Up;
                    self.record(Record::info(dlci, "建链成功"), now);
                }
                Action::Failed(dlci, why) => {
                    self.state.set_dlc_state(dlci, DlcState::Failed);
                    self.record(Record::error(dlci, format!("建链失败：{why}")), now);
                }
                Action::Closed(dlci) => {
                    self.state.set_dlc_state(dlci, DlcState::Closed);
                    self.record(Record::info(dlci, "通道已关闭"), now);
                }
                Action::Data(dlci, data) => {
                    self.record(Record::new(Direction::Rx, dlci, data), now);
                }
                Action::Control(msg, is_cmd) => {
                    let kind = if is_cmd { "命令" } else { "响应" };
                    self.state.notice = Some(format!("控制通道{kind}：{}", msg.describe()));
                }
            }
        }
    }

    /// 处理一个来自 I/O 或输入线程的事件。
    pub fn on_event(&mut self, ev: Event, now: Instant) {
        match ev {
            Event::Frame(f) => {
                self.state.stats.rx_frames += 1;
                self.state.push_raw(RawEntry::from_frame(&f, false));
                let acts = self.session.on_frame(&f, now);
                self.apply(acts, now);
            }
            Event::FcsError(raw) => {
                self.state.stats.fcs_errors += 1;
                self.state.push_raw(RawEntry::bad("FCS 不匹配", raw));
            }
            Event::Garbage(bytes) => {
                self.state.stats.garbage_bytes += bytes.len() as u64;
                self.state
                    .push_raw(RawEntry::bad(format!("无法成帧的 {} 字节", bytes.len()), bytes));
            }
            Event::SerialError(e) => {
                self.state.link = LinkState::Down;
                for slot in self.state.slots.iter_mut() {
                    slot.state = DlcState::Closed;
                }
                self.state.notice = Some(format!("串口错误：{e} — 请退出并重启工具"));
            }
            Event::Tick => {
                // 数据停止流入后，由心跳把最后一段累积冲刷出去。
                self.flush_if_stale(now);
                let acts = self.session.tick(now);
                self.apply(acts, now);
            }
            Event::Key(_) => {
                // 按键由主循环先经 keymap 处理，不会走到这里。
            }
        }
    }

    /// 执行一条来自按键映射的命令。
    pub fn exec(&mut self, cmd: Command, now: Instant) {
        match cmd {
            Command::None => {}
            Command::Quit => self.running = false,
            Command::Send { dlci, bytes } => {
                if self.session.state(dlci) != DlcState::Open {
                    self.record(Record::error(dlci, "通道未建链，无法发送"), now);
                    return;
                }
                self.record(Record::new(Direction::Tx, dlci, bytes.clone()), now);
                let acts = self.session.send_data(dlci, bytes);
                self.apply(acts, now);
            }
            Command::OpenDlc { dlci, slot } => {
                // 先让槽位归位，再发起建链。
                if let Some(existing) = self.state.slots[slot].dlci
                    && existing != dlci
                {
                    self.state.unbind(existing);
                }
                if self.state.bind(dlci).is_none() {
                    self.state.notice = Some("两个槽位都已占用，请先关闭一个".into());
                    return;
                }
                self.state
                    .set_dlc_state(dlci, DlcState::Opening { attempts: 1 });
                self.record(Record::info(dlci, "发起建链 (SABM)"), now);
                let acts = self.session.open(dlci, now);
                self.apply(acts, now);
            }
            Command::CloseDlc { dlci } => {
                self.record(Record::info(dlci, "请求关闭通道 (DISC)"), now);
                let acts = self.session.close(dlci, now);
                self.apply(acts, now);
            }
            Command::SendMsc { dlci } => {
                let msg = ControlMessage::Msc { dlci, signals: MSC_SIGNALS_READY };
                self.state.notice = Some(format!("已发送 {}", msg.describe()));
                self.send_frame(Frame::uih(0, msg.encode_command()));
            }
            Command::CloseMux => {
                self.state.notice = Some("已发送 CLD，复用即将关闭".into());
                self.send_frame(Frame::uih(0, ControlMessage::Cld.encode_command()));
            }
            Command::ToggleLog => self.toggle_log(),
        }
    }

    /// 开关会话日志。
    fn toggle_log(&mut self) {
        if self.log.is_some() {
            if let Some(l) = self.log.as_mut() {
                let _ = l.flush();
            }
            self.log = None;
            self.state.notice = Some("已停止记录日志".into());
            return;
        }
        let path = self.log_path.clone().unwrap_or_else(|| {
            PathBuf::from(format!(
                "cmux-{}.log",
                chrono::Local::now().format("%Y%m%d-%H%M%S")
            ))
        });
        match SessionLog::open(&path) {
            Ok(l) => {
                self.log = Some(l);
                self.log_path = Some(path.clone());
                self.state.notice = Some(format!("正在记录到 {}", path.display()));
            }
            Err(e) => {
                self.state.notice = Some(format!("无法打开日志文件：{e}"));
            }
        }
    }

    /// 退出前的收尾：冲刷未落地的数据，关闭已建链通道并关掉复用。
    pub fn shutdown(&mut self, now: Instant) {
        self.flush_pending();
        for dlci in self.session.open_dlcis() {
            let acts = self.session.close(dlci, now);
            self.apply(acts, now);
        }
        self.send_frame(Frame::uih(0, ControlMessage::Cld.encode_command()));
        if let Some(l) = self.log.as_mut() {
            let _ = l.flush();
        }
        let _ = self.tx.send(TxRequest::Shutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::{AppConfig, LinkState};
    use crate::mux::frame::{Frame, FrameType};
    use crate::mux::session::{DlcState, SessionConfig};
    use std::sync::mpsc::{self, Receiver};
    use std::time::Instant;

    fn setup() -> (Runtime, Receiver<TxRequest>) {
        let (tx, rx) = mpsc::channel();
        let rt = Runtime::new(AppConfig::default(), SessionConfig::default(), tx);
        (rt, rx)
    }

    /// 取出写线程队列里的全部帧。
    fn sent_frames(rx: &Receiver<TxRequest>) -> Vec<Frame> {
        rx.try_iter()
            .filter_map(|r| match r {
                TxRequest::Frame(f) => Some(f),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn opening_a_dlc_sends_sabm_and_binds_the_slot() {
        let (mut rt, rx) = setup();
        rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, Instant::now());

        let frames = sent_frames(&rx);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].ftype, FrameType::Sabm);
        assert_eq!(rt.state.slots[0].dlci, Some(1));
        assert!(matches!(rt.state.slots[0].state, DlcState::Opening { .. }));
    }

    #[test]
    fn ua_marks_the_channel_up_and_logs_a_notice() {
        let (mut rt, _rx) = setup();
        let now = Instant::now();
        rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, now);
        rt.on_event(Event::Frame(Frame::ua(1)), now);

        assert_eq!(rt.state.slots[0].state, DlcState::Open);
        let last = rt.state.slots[0].records.back().unwrap();
        assert!(
            last.note.as_ref().unwrap().contains("建链成功"),
            "实际: {last:?}"
        );
    }

    #[test]
    fn incoming_uih_becomes_an_rx_record() {
        let (mut rt, _rx) = setup();
        let now = Instant::now();
        rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, now);
        rt.on_event(Event::Frame(Frame::ua(1)), now);
        rt.on_event(Event::Frame(Frame::uih(1, b"OK\r\n".to_vec())), now);
        // 数据先进合并缓冲，窗口过后由心跳冲刷出来
        rt.on_event(Event::Tick, now + std::time::Duration::from_millis(50));

        let last = rt.state.slots[0].records.back().unwrap();
        assert_eq!(last.bytes, b"OK\r\n");
        assert_eq!(last.dir, crate::app::record::Direction::Rx);
        assert_eq!(rt.state.stats.rx_frames, 2, "UA 与 UIH 都计入收帧数");
    }

    #[test]
    fn sending_records_a_tx_entry_and_queues_a_frame() {
        let (mut rt, rx) = setup();
        let now = Instant::now();
        rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, now);
        rt.on_event(Event::Frame(Frame::ua(1)), now);
        let _ = sent_frames(&rx); // 清掉 SABM

        rt.exec(Command::Send { dlci: 1, bytes: b"AT\r".to_vec() }, now);

        let frames = sent_frames(&rx);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].ftype, FrameType::Uih);
        assert_eq!(frames[0].info, b"AT\r");
        // 发出的数据同样先进合并缓冲
        rt.on_event(Event::Tick, now + std::time::Duration::from_millis(50));
        let last = rt.state.slots[0].records.back().unwrap();
        assert_eq!(last.dir, crate::app::record::Direction::Tx);
        assert_eq!(last.bytes, b"AT\r");
        assert_eq!(rt.state.stats.tx_frames, 2, "SABM 与 UIH 都计入发帧数");
    }

    #[test]
    fn sending_on_a_channel_that_is_not_open_is_refused() {
        let (mut rt, rx) = setup();
        rt.state.bind(1);
        rt.exec(
            Command::Send { dlci: 1, bytes: b"AT\r".to_vec() },
            Instant::now(),
        );

        assert!(sent_frames(&rx).is_empty(), "未建链不应真的发出去");
        let last = rt.state.slots[0].records.back().unwrap();
        assert_eq!(last.dir, crate::app::record::Direction::Error);
    }

    #[test]
    fn successful_dlc_open_corrects_a_stale_mux_down() {
        // 模块已在 CMUX 模式时不认裸 AT，握手会失败并把链路标成 Down，
        // 但随后 SABM/UA 成功就证明复用是通的，状态栏必须跟着纠正。
        let (mut rt, _rx) = setup();
        rt.state.link = LinkState::Down;
        let now = Instant::now();
        rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, now);
        rt.on_event(Event::Frame(Frame::ua(1)), now);
        assert_eq!(rt.state.link, LinkState::Up, "建链成功后不该还显示 MUX DOWN");
    }

    #[test]
    fn dm_marks_the_channel_failed_with_a_reason() {
        let (mut rt, _rx) = setup();
        let now = Instant::now();
        rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, now);
        let dm = Frame { dlci: 1, cr: false, pf: true, ftype: FrameType::Dm, info: vec![] };
        rt.on_event(Event::Frame(dm), now);

        assert_eq!(rt.state.slots[0].state, DlcState::Failed);
        let last = rt.state.slots[0].records.back().unwrap();
        assert!(last.note.as_ref().unwrap().contains("DM"), "实际: {last:?}");
    }

    #[test]
    fn fcs_error_increments_counter_and_lands_in_raw_log() {
        let (mut rt, _rx) = setup();
        rt.on_event(
            Event::FcsError(vec![0xF9, 0x03, 0x3F, 0x01, 0x00]),
            Instant::now(),
        );
        assert_eq!(rt.state.stats.fcs_errors, 1);
        assert_eq!(rt.state.raw_log.len(), 1);
        assert!(!rt.state.raw_log.back().unwrap().ok);
    }

    #[test]
    fn garbage_bytes_are_counted_and_shown_in_raw_log() {
        let (mut rt, _rx) = setup();
        rt.on_event(Event::Garbage(vec![0x11, 0x22, 0x33]), Instant::now());
        assert_eq!(rt.state.stats.garbage_bytes, 3);
        assert_eq!(rt.state.raw_log.len(), 1);
    }

    #[test]
    fn serial_error_takes_the_link_down_and_downs_all_channels() {
        let (mut rt, _rx) = setup();
        let now = Instant::now();
        rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, now);
        rt.on_event(Event::Frame(Frame::ua(1)), now);
        assert_eq!(rt.state.slots[0].state, DlcState::Open);

        rt.on_event(Event::SerialError("设备已拔出".into()), now);

        assert_eq!(rt.state.link, LinkState::Down);
        assert_eq!(rt.state.slots[0].state, DlcState::Closed);
        let notice = rt.state.notice.as_ref().unwrap();
        assert!(notice.contains("设备已拔出"), "实际: {notice}");
        assert!(notice.contains("重启"), "应告诉用户下一步怎么办，实际: {notice}");
    }

    #[test]
    fn tick_retransmits_sabm_after_the_timeout() {
        let (tx, rx) = mpsc::channel();
        let cfg = SessionConfig {
            open_timeout: std::time::Duration::from_millis(10),
            max_attempts: 3,
        };
        let mut rt = Runtime::new(AppConfig::default(), cfg, tx);
        let t0 = Instant::now();
        rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, t0);
        let _ = sent_frames(&rx);

        rt.on_event(Event::Tick, t0 + std::time::Duration::from_millis(20));

        let frames = sent_frames(&rx);
        assert_eq!(frames.len(), 1, "超时应重发 SABM");
        assert_eq!(frames[0].ftype, FrameType::Sabm);
    }

    #[test]
    fn close_mux_sends_cld_on_the_control_channel() {
        let (mut rt, rx) = setup();
        rt.exec(Command::CloseMux, Instant::now());
        let frames = sent_frames(&rx);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].dlci, 0, "CLD 走控制通道");
        assert_eq!(frames[0].ftype, FrameType::Uih);
        assert_eq!(
            frames[0].info,
            crate::mux::control::ControlMessage::Cld.encode_command()
        );
    }

    #[test]
    fn close_dlc_sends_disc() {
        let (mut rt, rx) = setup();
        let now = Instant::now();
        rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, now);
        rt.on_event(Event::Frame(Frame::ua(1)), now);
        let _ = sent_frames(&rx);

        rt.exec(Command::CloseDlc { dlci: 1 }, now);
        let frames = sent_frames(&rx);
        assert_eq!(frames[0].ftype, FrameType::Disc);
    }

    #[test]
    fn msc_is_sent_on_the_control_channel_for_the_target_dlci() {
        let (mut rt, rx) = setup();
        rt.exec(Command::SendMsc { dlci: 3 }, Instant::now());
        let frames = sent_frames(&rx);
        assert_eq!(frames[0].dlci, 0);
        let expected =
            crate::mux::control::ControlMessage::Msc { dlci: 3, signals: 0x8D }.encode_command();
        assert_eq!(frames[0].info, expected);
    }

    #[test]
    fn quit_stops_the_loop() {
        let (mut rt, _rx) = setup();
        assert!(rt.running);
        rt.exec(Command::Quit, Instant::now());
        assert!(!rt.running);
    }

    #[test]
    fn control_messages_from_peer_are_shown_as_notice() {
        let (mut rt, _rx) = setup();
        let info = crate::mux::control::ControlMessage::Cld.encode_command();
        rt.on_event(Event::Frame(Frame::uih(0, info)), Instant::now());
        assert!(
            rt.state.notice.as_ref().unwrap().contains("CLD"),
            "实际: {:?}",
            rt.state.notice
        );
    }

    #[test]
    fn every_frame_is_mirrored_into_the_raw_log() {
        let (mut rt, _rx) = setup();
        let now = Instant::now();
        rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, now);
        rt.on_event(Event::Frame(Frame::ua(1)), now);
        // 发出的 SABM 与收到的 UA 都应留痕
        assert_eq!(rt.state.raw_log.len(), 2);
        assert!(rt.state.raw_log[0].outgoing);
        assert!(!rt.state.raw_log[1].outgoing);
    }

    /// 建好 DLCI 1 的链路，返回运行时与写线程接收端。
    fn opened() -> (Runtime, Receiver<TxRequest>, Instant) {
        let (mut rt, rx) = setup();
        let now = Instant::now();
        rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, now);
        rt.on_event(Event::Frame(Frame::ua(1)), now);
        (rt, rx, now)
    }

    fn rx_records(rt: &Runtime) -> Vec<Vec<u8>> {
        rt.state.slots[0]
            .records
            .iter()
            .filter(|r| r.dir == crate::app::record::Direction::Rx)
            .map(|r| r.bytes.clone())
            .collect()
    }

    #[test]
    fn byte_at_a_time_frames_merge_into_one_record() {
        // 复现 Quectel EC800M 的行为：逐字节成帧
        let (mut rt, _tx, now) = opened();
        for (i, b) in b"ATI".iter().enumerate() {
            rt.on_event(
                Event::Frame(Frame::uih(1, vec![*b])),
                now + std::time::Duration::from_millis(i as u64 * 2),
            );
        }
        // 窗口过后由 tick 冲刷
        rt.on_event(Event::Tick, now + std::time::Duration::from_millis(50));

        assert_eq!(rx_records(&rt), vec![b"ATI".to_vec()], "三个 1 字节帧应合成一条");
    }

    #[test]
    fn merged_record_keeps_the_first_frames_timestamp() {
        let (mut rt, _tx, now) = opened();
        rt.on_event(Event::Frame(Frame::uih(1, b"A".to_vec())), now);
        let first_ts = rt.pending.as_ref().expect("应在累积中").ts;
        rt.on_event(
            Event::Frame(Frame::uih(1, b"B".to_vec())),
            now + std::time::Duration::from_millis(2),
        );
        rt.on_event(Event::Tick, now + std::time::Duration::from_millis(50));

        let rec = rt.state.slots[0].records.back().unwrap();
        assert_eq!(rec.bytes, b"AB");
        assert_eq!(rec.ts, first_ts, "时间戳应是首帧到达的时刻");
    }

    #[test]
    fn frames_beyond_the_window_stay_separate() {
        let (mut rt, _tx, now) = opened();
        rt.on_event(Event::Frame(Frame::uih(1, b"A".to_vec())), now);
        // 间隔超过默认 10ms 窗口
        rt.on_event(
            Event::Frame(Frame::uih(1, b"B".to_vec())),
            now + std::time::Duration::from_millis(30),
        );
        rt.on_event(Event::Tick, now + std::time::Duration::from_millis(100));

        assert_eq!(rx_records(&rt), vec![b"A".to_vec(), b"B".to_vec()]);
    }

    #[test]
    fn different_channels_never_merge() {
        let (mut rt, _tx, now) = opened();
        rt.exec(Command::OpenDlc { dlci: 3, slot: 1 }, now);
        rt.on_event(Event::Frame(Frame::ua(3)), now);

        rt.on_event(Event::Frame(Frame::uih(1, b"A".to_vec())), now);
        rt.on_event(Event::Frame(Frame::uih(3, b"B".to_vec())), now);
        rt.on_event(Event::Tick, now + std::time::Duration::from_millis(50));

        assert_eq!(rx_records(&rt), vec![b"A".to_vec()], "通道 1 只该有自己的数据");
        let slot1: Vec<_> = rt.state.slots[1]
            .records
            .iter()
            .filter(|r| r.dir == crate::app::record::Direction::Rx)
            .map(|r| r.bytes.clone())
            .collect();
        assert_eq!(slot1, vec![b"B".to_vec()]);
    }

    #[test]
    fn tx_and_rx_never_merge_into_each_other() {
        let (mut rt, _tx, now) = opened();
        rt.on_event(Event::Frame(Frame::uih(1, b"A".to_vec())), now);
        rt.exec(Command::Send { dlci: 1, bytes: b"B".to_vec() }, now);
        rt.on_event(Event::Tick, now + std::time::Duration::from_millis(50));

        let dirs: Vec<_> = rt.state.slots[0]
            .records
            .iter()
            .filter(|r| r.note.is_none())
            .map(|r| (r.dir, r.bytes.clone()))
            .collect();
        assert_eq!(
            dirs,
            vec![
                (crate::app::record::Direction::Rx, b"A".to_vec()),
                (crate::app::record::Direction::Tx, b"B".to_vec()),
            ]
        );
    }

    #[test]
    fn a_notice_flushes_whatever_was_accumulating() {
        // 提示类记录必须保持时序：不能插到还没冲刷的数据前面
        let (mut rt, _tx, now) = opened();
        rt.on_event(Event::Frame(Frame::uih(1, b"A".to_vec())), now);
        rt.exec(Command::CloseDlc { dlci: 1 }, now);

        let kinds: Vec<_> = rt.state.slots[0]
            .records
            .iter()
            .map(|r| r.note.is_some())
            .collect();
        // …建链成功(note) → 数据(无 note) → 请求关闭(note)
        assert_eq!(kinds.last(), Some(&true), "最后应是关闭提示");
        assert!(
            !kinds[kinds.len() - 2],
            "关闭提示之前应先冲刷出累积的数据"
        );
    }

    #[test]
    fn shutdown_flushes_pending_data() {
        let (mut rt, _tx, now) = opened();
        rt.on_event(Event::Frame(Frame::uih(1, b"AT".to_vec())), now);
        rt.shutdown(now);
        assert_eq!(rx_records(&rt), vec![b"AT".to_vec()], "退出前应冲刷未落盘的数据");
    }

    #[test]
    fn zero_window_disables_merging() {
        let (tx, _rx) = mpsc::channel();
        let cfg = AppConfig { merge_window: std::time::Duration::ZERO, ..AppConfig::default() };
        let mut rt = Runtime::new(cfg, SessionConfig::default(), tx);
        let now = Instant::now();
        rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, now);
        rt.on_event(Event::Frame(Frame::ua(1)), now);

        rt.on_event(Event::Frame(Frame::uih(1, b"A".to_vec())), now);
        rt.on_event(Event::Frame(Frame::uih(1, b"B".to_vec())), now);

        assert_eq!(
            rx_records(&rt),
            vec![b"A".to_vec(), b"B".to_vec()],
            "窗口为零时应逐帧显示，且不经缓冲立即可见"
        );
    }

    #[test]
    fn shutdown_closes_channels_and_sends_cld() {
        let (mut rt, rx) = setup();
        let now = Instant::now();
        rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, now);
        rt.on_event(Event::Frame(Frame::ua(1)), now);
        let _ = sent_frames(&rx);

        rt.shutdown(now);

        let frames = sent_frames(&rx);
        assert!(
            frames.iter().any(|f| f.ftype == FrameType::Disc),
            "应对已建链通道发 DISC"
        );
        assert!(
            frames
                .iter()
                .any(|f| f.dlci == 0 && f.ftype == FrameType::Uih),
            "应在控制通道发 CLD"
        );
    }
}
