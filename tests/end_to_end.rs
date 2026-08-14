//! 端到端回环：用内存流扮演对端模块，跑完整条链路。
//!
//! 覆盖「AT 握手 → 双通道建链 → 双向收发 → 拆链」，把各层单元测试
//! 之间的接缝也串起来验证。协议细节由各模块的单元测试保证，这里只关心
//! 组合起来是否真的能工作。

use cmux_tui::app::event::{Event, TxRequest};
use cmux_tui::app::keymap::Command;
use cmux_tui::app::record::Direction;
use cmux_tui::app::runtime::Runtime;
use cmux_tui::app::state::AppConfig;
use cmux_tui::mux::decoder::{DecodeEvent, FrameDecoder};
use cmux_tui::mux::frame::{Frame, FrameType};
use cmux_tui::mux::session::{DlcState, SessionConfig};
use cmux_tui::serial::at::{AtConfig, enter_mux};
use cmux_tui::serial::stream::MockStream;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// 取出工具发往串口的所有帧，即「对端会收到什么」。
fn sent(rx: &Receiver<TxRequest>) -> Vec<Frame> {
    rx.try_iter()
        .filter_map(|r| match r {
            TxRequest::Frame(f) => Some(f),
            _ => None,
        })
        .collect()
}

/// 模拟对端发来一段字节：经解码器切帧后喂给运行时。
fn peer_sends(rt: &mut Runtime, dec: &mut FrameDecoder, bytes: &[u8], now: Instant) {
    for ev in dec.push(bytes) {
        let event = match ev {
            DecodeEvent::Frame(f) => Event::Frame(f),
            DecodeEvent::FcsError { raw } => Event::FcsError(raw),
            DecodeEvent::Garbage(b) => Event::Garbage(b),
        };
        rt.on_event(event, now);
    }
}

fn new_runtime() -> (Runtime, Receiver<TxRequest>, FrameDecoder) {
    let (tx, rx) = mpsc::channel();
    let rt = Runtime::new(AppConfig::default(), SessionConfig::default(), tx);
    (rt, rx, FrameDecoder::new(127))
}

#[test]
fn at_handshake_puts_the_module_into_mux_mode() {
    let port = MockStream::new();
    port.feed(b"\r\nOK\r\n"); // 回应 AT
    port.feed(b"\r\nOK\r\n"); // 回应 AT+CMUX
    let mut stream = port.clone();

    enter_mux(&mut stream, &AtConfig::default()).expect("握手应成功");

    let written = String::from_utf8_lossy(&port.written()).to_string();
    assert!(written.starts_with("AT\r"), "实际: {written:?}");
    assert!(written.contains("AT+CMUX=0,0,5,127,"), "实际: {written:?}");
}

#[test]
fn two_channels_open_and_exchange_data_independently() {
    let (mut rt, rx, mut dec) = new_runtime();
    let now = Instant::now();

    // 两个槽位分别绑定 DLCI 1 与 DLCI 3，各发一个 SABM。
    rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, now);
    rt.exec(Command::OpenDlc { dlci: 3, slot: 1 }, now);
    let frames = sent(&rx);
    assert_eq!(frames.len(), 2);
    assert!(frames.iter().all(|f| f.ftype == FrameType::Sabm));
    assert_eq!(frames[0].dlci, 1);
    assert_eq!(frames[1].dlci, 3);

    // 对端对两个通道都回 UA。
    peer_sends(&mut rt, &mut dec, &Frame::ua(1).encode(), now);
    peer_sends(&mut rt, &mut dec, &Frame::ua(3).encode(), now);
    assert_eq!(rt.state.slots[0].state, DlcState::Open);
    assert_eq!(rt.state.slots[1].state, DlcState::Open);

    // 在通道 1 上发 AT 命令。
    rt.exec(
        Command::Send { dlci: 1, bytes: b"AT+CGSN\r".to_vec() },
        now,
    );
    let frames = sent(&rx);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].dlci, 1);
    assert_eq!(frames[0].info, b"AT+CGSN\r");

    // 对端在通道 1 回数据，在通道 3 回另一份数据。
    peer_sends(
        &mut rt,
        &mut dec,
        &Frame::uih(1, b"861234567890\r\n".to_vec()).encode(),
        now,
    );
    peer_sends(
        &mut rt,
        &mut dec,
        &Frame::uih(3, b"+CSQ: 20,99\r\n".to_vec()).encode(),
        now,
    );

    // 数据先进合并缓冲，窗口过后由心跳冲刷
    rt.on_event(Event::Tick, now + Duration::from_millis(50));

    // 两个通道的数据必须各归各的槽位，不能串台。
    let slot0 = rt.state.slots[0].records.back().unwrap();
    assert_eq!(slot0.dir, Direction::Rx);
    assert_eq!(slot0.bytes, b"861234567890\r\n");

    let slot1 = rt.state.slots[1].records.back().unwrap();
    assert_eq!(slot1.dir, Direction::Rx);
    assert_eq!(slot1.bytes, b"+CSQ: 20,99\r\n");

    assert_eq!(rt.state.unrouted, 0, "不应有记录落到未绑定的通道");
}

#[test]
fn peer_data_on_an_unbound_channel_is_counted_not_lost_silently() {
    let (mut rt, _rx, mut dec) = new_runtime();
    let now = Instant::now();
    rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, now);
    peer_sends(&mut rt, &mut dec, &Frame::ua(1).encode(), now);

    // DLCI 5 没有绑定到任何槽位
    peer_sends(&mut rt, &mut dec, &Frame::uih(5, b"x".to_vec()).encode(), now);
    rt.on_event(Event::Tick, now + Duration::from_millis(50));

    assert_eq!(rt.state.unrouted, 1);
    assert_eq!(rt.state.stats.rx_frames, 2);
}

#[test]
fn corrupted_frames_are_counted_and_the_stream_resyncs() {
    let (mut rt, _rx, mut dec) = new_runtime();
    let now = Instant::now();
    rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, now);

    // 先送一个 FCS 被破坏的 UA，再送一个完好的 UA。
    let mut bad = Frame::ua(1).encode();
    let n = bad.len();
    bad[n - 2] ^= 0xFF;
    peer_sends(&mut rt, &mut dec, &bad, now);
    assert_eq!(rt.state.stats.fcs_errors, 1);
    assert_ne!(rt.state.slots[0].state, DlcState::Open, "坏帧不应建链");

    peer_sends(&mut rt, &mut dec, &Frame::ua(1).encode(), now);
    assert_eq!(rt.state.slots[0].state, DlcState::Open, "坏帧后应能重新同步");
}

#[test]
fn closing_a_channel_completes_the_disc_ua_exchange() {
    let (mut rt, rx, mut dec) = new_runtime();
    let now = Instant::now();
    rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, now);
    peer_sends(&mut rt, &mut dec, &Frame::ua(1).encode(), now);
    let _ = sent(&rx);

    rt.exec(Command::CloseDlc { dlci: 1 }, now);
    let frames = sent(&rx);
    assert_eq!(frames[0].ftype, FrameType::Disc);

    peer_sends(&mut rt, &mut dec, &Frame::ua(1).encode(), now);
    assert_eq!(rt.state.slots[0].state, DlcState::Closed);
}

#[test]
fn every_exchanged_frame_shows_up_in_the_raw_log() {
    let (mut rt, _rx, mut dec) = new_runtime();
    let now = Instant::now();
    rt.exec(Command::OpenDlc { dlci: 1, slot: 0 }, now);
    peer_sends(&mut rt, &mut dec, &Frame::ua(1).encode(), now);
    rt.exec(Command::Send { dlci: 1, bytes: b"AT\r".to_vec() }, now);

    // SABM(发) + UA(收) + UIH(发)
    assert_eq!(rt.state.raw_log.len(), 3);
    let summaries: Vec<String> = rt.state.raw_log.iter().map(|e| e.line()).collect();
    assert!(summaries[0].contains("SABM"), "实际: {:?}", summaries);
    assert!(summaries[1].contains("UA"), "实际: {:?}", summaries);
    assert!(summaries[2].contains("UIH"), "实际: {:?}", summaries);
}
