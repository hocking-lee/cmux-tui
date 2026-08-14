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

pub struct Runtime {
    pub state: AppState,
    pub session: Session,
    pub tx: Sender<TxRequest>,
    pub log: Option<SessionLog>,
    pub log_path: Option<PathBuf>,
    pub running: bool,
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
        }
    }

    /// 记录一条内容并同步写日志。
    fn record(&mut self, rec: Record) {
        if let Some(l) = self.log.as_mut() {
            let _ = l.write_record(&rec);
        }
        self.state.push_record(rec);
    }

    /// 把一帧投递给写线程，并记入原始帧日志与计数。
    fn send_frame(&mut self, f: Frame) {
        self.state.push_raw(RawEntry::from_frame(&f, true));
        self.state.stats.tx_frames += 1;
        let _ = self.tx.send(TxRequest::Frame(f));
    }

    /// 执行状态机产出的动作。
    fn apply(&mut self, actions: Vec<Action>) {
        for act in actions {
            match act {
                Action::Send(f) => self.send_frame(f),
                Action::Opened(dlci) => {
                    self.state.set_dlc_state(dlci, DlcState::Open);
                    self.record(Record::info(dlci, "建链成功"));
                }
                Action::Failed(dlci, why) => {
                    self.state.set_dlc_state(dlci, DlcState::Failed);
                    self.record(Record::error(dlci, format!("建链失败：{why}")));
                }
                Action::Closed(dlci) => {
                    self.state.set_dlc_state(dlci, DlcState::Closed);
                    self.record(Record::info(dlci, "通道已关闭"));
                }
                Action::Data(dlci, data) => {
                    self.record(Record::new(Direction::Rx, dlci, data));
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
                self.apply(acts);
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
                let acts = self.session.tick(now);
                self.apply(acts);
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
                    self.record(Record::error(dlci, "通道未建链，无法发送"));
                    return;
                }
                self.record(Record::new(Direction::Tx, dlci, bytes.clone()));
                let acts = self.session.send_data(dlci, bytes);
                self.apply(acts);
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
                self.record(Record::info(dlci, "发起建链 (SABM)"));
                let acts = self.session.open(dlci, now);
                self.apply(acts);
            }
            Command::CloseDlc { dlci } => {
                self.record(Record::info(dlci, "请求关闭通道 (DISC)"));
                let acts = self.session.close(dlci, now);
                self.apply(acts);
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

    /// 退出前的收尾：关闭已建链通道并关掉复用。
    pub fn shutdown(&mut self, now: Instant) {
        for dlci in self.session.open_dlcis() {
            let acts = self.session.close(dlci, now);
            self.apply(acts);
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
