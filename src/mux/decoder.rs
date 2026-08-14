//! 把串口字节流增量切分成帧。
//!
//! 设计要点：解码器永远不会因为坏数据卡死。任何解析失败都退回到
//! 「寻找下一个标志字节」状态，并把已消费的字节作为事件报告出去，
//! 供原始帧视图展示。

use crate::mux::fcs;
use crate::mux::frame::{FLAG, Frame, FrameType};

/// 解码器产生的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeEvent {
    /// 成功解出一帧。
    Frame(Frame),
    /// 帧结构完整但 FCS 不匹配，附上原始字节。
    FcsError { raw: Vec<u8> },
    /// 无法成帧被丢弃的字节。
    Garbage(Vec<u8>),
}

/// 增量帧解码状态机。
pub struct FrameDecoder {
    max_info: usize,
    /// 当前正在累积的候选帧（不含前导标志）。
    buf: Vec<u8>,
    /// 是否已经见到起始标志。
    in_frame: bool,
    /// 尚未成帧的垃圾字节。
    garbage: Vec<u8>,
}

impl FrameDecoder {
    pub fn new(max_info: usize) -> Self {
        FrameDecoder {
            max_info,
            buf: Vec::with_capacity(512),
            in_frame: false,
            garbage: Vec::new(),
        }
    }

    /// 喂入一段字节，返回本次产生的所有事件。
    pub fn push(&mut self, bytes: &[u8]) -> Vec<DecodeEvent> {
        let mut events = Vec::new();
        for &b in bytes {
            if !self.in_frame {
                if b == FLAG {
                    self.in_frame = true;
                    self.buf.clear();
                    self.flush_garbage(&mut events);
                } else {
                    self.garbage.push(b);
                    if self.garbage.len() >= 256 {
                        self.flush_garbage(&mut events);
                    }
                }
                continue;
            }

            if b == FLAG {
                if self.buf.is_empty() {
                    // 连续标志或共用标志，保持在帧起始状态。
                    continue;
                }
                let raw = std::mem::take(&mut self.buf);
                self.decode_body(raw, &mut events);
                // 这个标志同时是下一帧的起始。
                self.in_frame = true;
                continue;
            }

            self.buf.push(b);

            // 防止畸形长度字段导致无限累积。
            if self.buf.len() > self.max_info + 8 {
                let raw = std::mem::take(&mut self.buf);
                events.push(DecodeEvent::Garbage(raw));
                self.in_frame = false;
            }
        }
        events
    }

    fn flush_garbage(&mut self, events: &mut Vec<DecodeEvent>) {
        if !self.garbage.is_empty() {
            events.push(DecodeEvent::Garbage(std::mem::take(&mut self.garbage)));
        }
    }

    /// 解析一个位于两个标志之间的帧体。
    fn decode_body(&mut self, raw: Vec<u8>, events: &mut Vec<DecodeEvent>) {
        // 最短合法帧体：地址 + 控制 + 长度 + FCS
        if raw.len() < 4 {
            events.push(DecodeEvent::Garbage(raw));
            return;
        }

        let address = raw[0];
        let control = raw[1];

        let (info_len, header_len) = if raw[2] & 0x01 != 0 {
            ((raw[2] >> 1) as usize, 3usize)
        } else {
            if raw.len() < 5 {
                events.push(DecodeEvent::Garbage(raw));
                return;
            }
            let len = ((raw[2] >> 1) as usize & 0x7F) | ((raw[3] as usize) << 7);
            (len, 4usize)
        };

        if info_len > self.max_info || raw.len() != header_len + info_len + 1 {
            events.push(DecodeEvent::Garbage(raw));
            return;
        }

        let Some((ftype, pf)) = FrameType::from_control(control) else {
            events.push(DecodeEvent::Garbage(raw));
            return;
        };

        let header = &raw[..header_len];
        let info = &raw[header_len..header_len + info_len];
        let received = raw[header_len + info_len];

        let ok = if ftype == FrameType::Uih {
            fcs::check(header, received)
        } else {
            let mut buf = header.to_vec();
            buf.extend_from_slice(info);
            fcs::check(&buf, received)
        };

        if !ok {
            events.push(DecodeEvent::FcsError { raw });
            return;
        }

        events.push(DecodeEvent::Frame(Frame {
            dlci: address >> 2,
            cr: address & 0x02 != 0,
            pf,
            ftype,
            info: info.to_vec(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::frame::{Frame, FrameType};

    fn frames(events: Vec<DecodeEvent>) -> Vec<Frame> {
        events
            .into_iter()
            .filter_map(|e| match e {
                DecodeEvent::Frame(f) => Some(f),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn decodes_a_whole_frame() {
        let mut d = FrameDecoder::new(1024);
        let wire = Frame::sabm(0).encode();
        let got = frames(d.push(&wire));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].ftype, FrameType::Sabm);
        assert_eq!(got[0].dlci, 0);
    }

    #[test]
    fn decodes_frame_split_across_pushes() {
        let mut d = FrameDecoder::new(1024);
        let wire = Frame::uih(1, b"hello".to_vec()).encode();
        let mut got = Vec::new();
        for chunk in wire.chunks(2) {
            got.extend(frames(d.push(chunk)));
        }
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].info, b"hello");
    }

    #[test]
    fn decodes_back_to_back_frames_in_one_push() {
        let mut d = FrameDecoder::new(1024);
        let mut wire = Frame::sabm(1).encode();
        wire.extend(Frame::uih(1, b"ok".to_vec()).encode());
        let got = frames(d.push(&wire));
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].info, b"ok");
    }

    #[test]
    fn tolerates_shared_flag_between_frames() {
        // 相邻帧共用一个 F9：...FCS F9 ADDR...
        let mut d = FrameDecoder::new(1024);
        let a = Frame::sabm(1).encode();
        let b = Frame::sabm(2).encode();
        let mut wire = a.clone();
        wire.extend_from_slice(&b[1..]); // 去掉 b 的前导 F9
        let got = frames(d.push(&wire));
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].dlci, 2);
    }

    #[test]
    fn tolerates_runs_of_flags() {
        let mut d = FrameDecoder::new(1024);
        let mut wire = vec![0xF9, 0xF9, 0xF9];
        wire.extend(Frame::sabm(1).encode());
        wire.extend_from_slice(&[0xF9, 0xF9]);
        let got = frames(d.push(&wire));
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn reports_fcs_error_without_losing_sync() {
        let mut d = FrameDecoder::new(1024);
        let mut bad = Frame::sabm(1).encode();
        let n = bad.len();
        bad[n - 2] ^= 0xFF; // 破坏 FCS
        bad.extend(Frame::sabm(2).encode());

        let events = d.push(&bad);
        assert!(
            events.iter().any(|e| matches!(e, DecodeEvent::FcsError { .. })),
            "应报告 FCS 错误"
        );
        let good = frames(events);
        assert_eq!(good.len(), 1, "错误帧之后必须重新同步");
        assert_eq!(good[0].dlci, 2);
    }

    #[test]
    fn decodes_two_byte_length_frame() {
        let mut d = FrameDecoder::new(4096);
        let info = vec![0x5Au8; 300];
        let wire = Frame::uih(1, info.clone()).encode();
        let got = frames(d.push(&wire));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].info, info);
    }

    #[test]
    fn oversized_length_is_rejected_and_resyncs() {
        let mut d = FrameDecoder::new(16); // max_info 很小
        let mut wire = Frame::uih(1, vec![0u8; 100]).encode();
        wire.extend(Frame::sabm(2).encode());
        let events = d.push(&wire);
        let good = frames(events);
        assert_eq!(good.len(), 1);
        assert_eq!(good[0].dlci, 2);
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        let mut d = FrameDecoder::new(256);
        // 确定性伪随机流，含大量 F9
        let mut x: u32 = 0x12345678;
        let data: Vec<u8> = (0..20000)
            .map(|_| {
                x = x.wrapping_mul(1103515245).wrapping_add(12345);
                let b = (x >> 16) as u8;
                if b.is_multiple_of(7) { 0xF9 } else { b }
            })
            .collect();
        for chunk in data.chunks(13) {
            let _ = d.push(chunk);
        }
    }

    #[test]
    fn unknown_control_byte_does_not_yield_frame() {
        let mut d = FrameDecoder::new(1024);
        // addr=03 ctrl=00(非法) len=01 fcs=任意
        let wire = [0xF9, 0x03, 0x00, 0x01, 0x00, 0xF9];
        let got = frames(d.push(&wire));
        assert!(got.is_empty());
    }
}
