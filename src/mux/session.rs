//! DLC 建链/拆链状态机。
//!
//! 本模块**不做任何 I/O**：调用方喂入收到的帧和当前时刻，
//! 它返回一组待执行的动作。这样超时与重试可以用受控时钟测试。

use crate::mux::control::ControlMessage;
use crate::mux::frame::{Frame, FrameType};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DlcState {
    Closed,
    Opening { attempts: u8 },
    Open,
    Closing { attempts: u8 },
    Failed,
}

/// 状态机要求调用方执行的动作。
#[derive(Debug, Clone)]
pub enum Action {
    /// 把这一帧发出去。
    Send(Frame),
    /// 指定 DLCI 建链成功。
    Opened(u8),
    /// 指定 DLCI 建链失败，附原因。
    Failed(u8, String),
    /// 指定 DLCI 已关闭。
    Closed(u8),
    /// 收到用户数据。
    Data(u8, Vec<u8>),
    /// 收到控制通道消息，bool 表示是否为命令。
    Control(ControlMessage, bool),
}

#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// 等待 UA 的超时。规范建议 T1=100ms，这里放宽以适应慢模块。
    pub open_timeout: Duration,
    /// 含首次发送在内的最大尝试次数。
    pub max_attempts: u8,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig { open_timeout: Duration::from_secs(3), max_attempts: 3 }
    }
}

struct Dlc {
    state: DlcState,
    deadline: Option<Instant>,
}

pub struct Session {
    cfg: SessionConfig,
    dlcs: BTreeMap<u8, Dlc>,
}

impl Session {
    pub fn new(cfg: SessionConfig) -> Session {
        Session { cfg, dlcs: BTreeMap::new() }
    }

    pub fn state(&self, dlci: u8) -> DlcState {
        self.dlcs
            .get(&dlci)
            .map(|d| d.state.clone())
            .unwrap_or(DlcState::Closed)
    }

    /// 当前处于 Open 状态的 DLCI 列表。
    pub fn open_dlcis(&self) -> Vec<u8> {
        self.dlcs
            .iter()
            .filter(|(_, d)| d.state == DlcState::Open)
            .map(|(k, _)| *k)
            .collect()
    }

    /// 发起建链。
    pub fn open(&mut self, dlci: u8, now: Instant) -> Vec<Action> {
        self.dlcs.insert(
            dlci,
            Dlc {
                state: DlcState::Opening { attempts: 1 },
                deadline: Some(now + self.cfg.open_timeout),
            },
        );
        vec![Action::Send(Frame::sabm(dlci))]
    }

    /// 发起拆链。
    pub fn close(&mut self, dlci: u8, now: Instant) -> Vec<Action> {
        self.dlcs.insert(
            dlci,
            Dlc {
                state: DlcState::Closing { attempts: 1 },
                deadline: Some(now + self.cfg.open_timeout),
            },
        );
        vec![Action::Send(Frame::disc(dlci))]
    }

    /// 在已建链的通道上发送数据。返回空表示通道不可用。
    pub fn send_data(&self, dlci: u8, data: Vec<u8>) -> Vec<Action> {
        if self.state(dlci) == DlcState::Open || dlci == 0 {
            vec![Action::Send(Frame::uih(dlci, data))]
        } else {
            Vec::new()
        }
    }

    /// 处理收到的一帧。
    pub fn on_frame(&mut self, f: &Frame, _now: Instant) -> Vec<Action> {
        let mut acts = Vec::new();
        match f.ftype {
            FrameType::Ua => {
                let entry = self.dlcs.entry(f.dlci).or_insert(Dlc {
                    state: DlcState::Closed,
                    deadline: None,
                });
                match entry.state {
                    DlcState::Opening { .. } => {
                        entry.state = DlcState::Open;
                        entry.deadline = None;
                        acts.push(Action::Opened(f.dlci));
                    }
                    DlcState::Closing { .. } => {
                        entry.state = DlcState::Closed;
                        entry.deadline = None;
                        acts.push(Action::Closed(f.dlci));
                    }
                    _ => {}
                }
            }
            FrameType::Dm => {
                let entry = self.dlcs.entry(f.dlci).or_insert(Dlc {
                    state: DlcState::Closed,
                    deadline: None,
                });
                entry.deadline = None;
                match entry.state {
                    DlcState::Closing { .. } => {
                        entry.state = DlcState::Closed;
                        acts.push(Action::Closed(f.dlci));
                    }
                    _ => {
                        entry.state = DlcState::Failed;
                        acts.push(Action::Failed(f.dlci, "对端返回 DM（拒绝建链）".into()));
                    }
                }
            }
            FrameType::Disc => {
                // 对端要求关闭：回 UA 并置为已关闭。
                acts.push(Action::Send(Frame::ua(f.dlci)));
                if let Some(d) = self.dlcs.get_mut(&f.dlci) {
                    d.state = DlcState::Closed;
                    d.deadline = None;
                }
                acts.push(Action::Closed(f.dlci));
            }
            FrameType::Uih => {
                if f.dlci == 0 {
                    if let Some((msg, is_cmd)) = ControlMessage::parse(&f.info) {
                        acts.push(Action::Control(msg, is_cmd));
                    }
                } else {
                    acts.push(Action::Data(f.dlci, f.info.clone()));
                }
            }
            FrameType::Sabm => {
                // 本工具是 initiator，但对端发来 SABM 时按规范回 UA。
                acts.push(Action::Send(Frame::ua(f.dlci)));
            }
        }
        acts
    }

    /// 推进超时。应当被主循环周期性调用。
    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        let mut acts = Vec::new();
        let max = self.cfg.max_attempts;
        let timeout = self.cfg.open_timeout;

        for (&dlci, dlc) in self.dlcs.iter_mut() {
            let Some(deadline) = dlc.deadline else { continue };
            if now < deadline {
                continue;
            }
            match dlc.state {
                DlcState::Opening { attempts } => {
                    if attempts >= max {
                        dlc.state = DlcState::Failed;
                        dlc.deadline = None;
                        acts.push(Action::Failed(
                            dlci,
                            format!("等待 UA 超时，已重试 {attempts} 次"),
                        ));
                    } else {
                        dlc.state = DlcState::Opening { attempts: attempts + 1 };
                        dlc.deadline = Some(now + timeout);
                        acts.push(Action::Send(Frame::sabm(dlci)));
                    }
                }
                DlcState::Closing { attempts } => {
                    if attempts >= max {
                        dlc.state = DlcState::Closed;
                        dlc.deadline = None;
                        acts.push(Action::Closed(dlci));
                    } else {
                        dlc.state = DlcState::Closing { attempts: attempts + 1 };
                        dlc.deadline = Some(now + timeout);
                        acts.push(Action::Send(Frame::disc(dlci)));
                    }
                }
                _ => {
                    dlc.deadline = None;
                }
            }
        }
        acts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::frame::{Frame, FrameType};
    use std::time::{Duration, Instant};

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn open_emits_sabm_and_enters_opening() {
        let mut s = Session::new(SessionConfig::default());
        let acts = s.open(1, t0());
        assert_eq!(acts.len(), 1);
        match &acts[0] {
            Action::Send(f) => {
                assert_eq!(f.ftype, FrameType::Sabm);
                assert_eq!(f.dlci, 1);
            }
            other => panic!("期望 Send(SABM)，得到 {other:?}"),
        }
        assert!(matches!(s.state(1), DlcState::Opening { .. }));
    }

    #[test]
    fn ua_completes_the_open() {
        let mut s = Session::new(SessionConfig::default());
        s.open(1, t0());
        let acts = s.on_frame(&Frame::ua(1), t0());
        assert_eq!(s.state(1), DlcState::Open);
        assert!(acts.iter().any(|a| matches!(a, Action::Opened(1))));
    }

    #[test]
    fn dm_fails_immediately_without_retry() {
        let mut s = Session::new(SessionConfig::default());
        s.open(1, t0());
        let dm = Frame { dlci: 1, cr: false, pf: true, ftype: FrameType::Dm, info: vec![] };
        let acts = s.on_frame(&dm, t0());
        assert_eq!(s.state(1), DlcState::Failed);
        assert!(acts.iter().any(|a| matches!(a, Action::Failed(1, _))));
        assert!(
            !acts.iter().any(|a| matches!(a, Action::Send(_))),
            "收到 DM 不应重试"
        );
    }

    #[test]
    fn timeout_retries_up_to_limit_then_fails() {
        let cfg = SessionConfig {
            open_timeout: Duration::from_millis(100),
            max_attempts: 3,
        };
        let mut s = Session::new(cfg);
        let start = t0();
        s.open(1, start);

        // 第 1、2 次超时应各重发一次 SABM
        let mut now = start;
        for i in 1..3 {
            now += Duration::from_millis(101);
            let acts = s.tick(now);
            assert!(
                acts.iter()
                    .any(|a| matches!(a, Action::Send(f) if f.ftype == FrameType::Sabm)),
                "第 {i} 次超时应重发 SABM"
            );
            assert!(matches!(s.state(1), DlcState::Opening { .. }));
        }

        // 第 3 次超时耗尽重试次数
        now += Duration::from_millis(101);
        let acts = s.tick(now);
        assert_eq!(s.state(1), DlcState::Failed);
        assert!(acts.iter().any(|a| matches!(a, Action::Failed(1, _))));
    }

    #[test]
    fn tick_does_nothing_before_deadline() {
        let cfg = SessionConfig {
            open_timeout: Duration::from_millis(100),
            ..SessionConfig::default()
        };
        let mut s = Session::new(cfg);
        let start = t0();
        s.open(1, start);
        let acts = s.tick(start + Duration::from_millis(50));
        assert!(acts.is_empty());
    }

    #[test]
    fn uih_on_open_channel_yields_data() {
        let mut s = Session::new(SessionConfig::default());
        s.open(1, t0());
        s.on_frame(&Frame::ua(1), t0());
        let acts = s.on_frame(&Frame::uih(1, b"hi".to_vec()), t0());
        assert!(acts.iter().any(|a| matches!(a, Action::Data(1, d) if d == b"hi")));
    }

    #[test]
    fn close_emits_disc_and_ua_confirms() {
        let mut s = Session::new(SessionConfig::default());
        s.open(1, t0());
        s.on_frame(&Frame::ua(1), t0());
        let acts = s.close(1, t0());
        assert!(
            acts.iter()
                .any(|a| matches!(a, Action::Send(f) if f.ftype == FrameType::Disc))
        );
        assert!(matches!(s.state(1), DlcState::Closing { .. }));

        s.on_frame(&Frame::ua(1), t0());
        assert_eq!(s.state(1), DlcState::Closed);
    }

    #[test]
    fn unknown_dlci_data_is_still_reported() {
        let mut s = Session::new(SessionConfig::default());
        let acts = s.on_frame(&Frame::uih(7, b"x".to_vec()), t0());
        assert!(acts.iter().any(|a| matches!(a, Action::Data(7, _))));
    }

    #[test]
    fn control_frame_on_dlci0_is_surfaced() {
        let mut s = Session::new(SessionConfig::default());
        let msg = crate::mux::control::ControlMessage::Cld;
        let acts = s.on_frame(&Frame::uih(0, msg.encode_command()), t0());
        assert!(
            acts.iter().any(|a| matches!(a, Action::Control(m, _) if *m == msg)),
            "DLCI 0 上的 UIH 应解析成控制消息"
        );
    }

    #[test]
    fn state_of_untouched_dlci_is_closed() {
        let s = Session::new(SessionConfig::default());
        assert_eq!(s.state(5), DlcState::Closed);
    }
}
