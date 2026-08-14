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

/// 解码状态机所处的阶段。
///
/// **必须按长度字段定界，不能扫描标志字节**。Basic 模式没有转义机制，
/// `0xF9` 可以合法地出现在 info 里，也可能恰好是算出来的 FCS
/// （实测 Quectel EC800M 对 AT+CPIN? 的应答就是：info 31 字节 →
/// 长度字节 0x3F → 头部 FCS 恰好等于 0xF9）。靠扫描标志定界的话，
/// 这类帧会被从中间截断而整帧丢失，且对固定命令是必现的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// 尚未同步，正在找起始标志。
    Hunting,
    /// 已见到起始标志，等待地址字节（可能先遇到连续的标志）。
    Address,
    /// 等待控制字节。
    Control,
    /// 等待长度字段第一字节。
    Length1,
    /// 等待长度字段第二字节（EA=0 时）。
    Length2,
    /// 按长度读取信息字段。
    Info,
    /// 等待 FCS 字节。
    Fcs,
    /// 等待结束标志。
    End,
}

/// 增量帧解码状态机。
pub struct FrameDecoder {
    max_info: usize,
    stage: Stage,
    /// 已收到的帧头字节（地址、控制、长度），用于计算 FCS。
    header: Vec<u8>,
    info: Vec<u8>,
    /// 由长度字段解析出的信息长度。
    info_len: usize,
    fcs: u8,
    /// 尚未成帧的垃圾字节。
    garbage: Vec<u8>,
}

impl FrameDecoder {
    pub fn new(max_info: usize) -> Self {
        FrameDecoder {
            max_info,
            stage: Stage::Hunting,
            header: Vec::with_capacity(4),
            info: Vec::with_capacity(256),
            info_len: 0,
            fcs: 0,
            garbage: Vec::new(),
        }
    }

    /// 喂入一段字节，返回本次产生的所有事件。
    pub fn push(&mut self, bytes: &[u8]) -> Vec<DecodeEvent> {
        let mut events = Vec::new();
        for &b in bytes {
            self.step(b, &mut events);
        }
        events
    }

    /// 处理一个字节。
    fn step(&mut self, b: u8, events: &mut Vec<DecodeEvent>) {
        match self.stage {
            Stage::Hunting => {
                if b == FLAG {
                    self.flush_garbage(events);
                    self.start_frame();
                } else {
                    self.push_garbage(b, events);
                }
            }
            // 相邻帧可以共用标志，帧前也可能有多个连续标志，这里一并吸收。
            Stage::Address => {
                if b == FLAG {
                    return;
                }
                self.header.push(b);
                self.stage = Stage::Control;
            }
            Stage::Control => {
                self.header.push(b);
                self.stage = Stage::Length1;
            }
            Stage::Length1 => {
                self.header.push(b);
                if b & 0x01 != 0 {
                    // EA=1：单字节长度
                    self.info_len = (b >> 1) as usize;
                    self.begin_info(events);
                } else {
                    self.stage = Stage::Length2;
                }
            }
            Stage::Length2 => {
                let low = (self.header[2] >> 1) as usize & 0x7F;
                self.header.push(b);
                self.info_len = low | ((b as usize) << 7);
                self.begin_info(events);
            }
            Stage::Info => {
                // 关键：按长度收字节，不看它是不是标志字节。
                self.info.push(b);
                if self.info.len() == self.info_len {
                    self.stage = Stage::Fcs;
                }
            }
            Stage::Fcs => {
                // 同样：FCS 可能恰好等于标志字节，照收不误。
                self.fcs = b;
                self.stage = Stage::End;
            }
            Stage::End => {
                if b == FLAG {
                    self.finish_frame(events);
                    // 这个标志同时可以充当下一帧的起始。
                    self.start_frame();
                } else {
                    // 该出现结束标志的位置却是别的字节，说明前面对错了，
                    // 把已消费的内容作为垃圾报出去并重新同步。
                    self.discard(events);
                    self.push_garbage(b, events);
                }
            }
        }
    }

    /// 进入新帧的接收状态。
    fn start_frame(&mut self) {
        self.stage = Stage::Address;
        self.header.clear();
        self.info.clear();
        self.info_len = 0;
    }

    /// 长度字段读完后，决定下一步。
    fn begin_info(&mut self, events: &mut Vec<DecodeEvent>) {
        if self.info_len > self.max_info {
            // 长度字段离谱，多半是把垃圾当成了帧头。
            self.discard(events);
            return;
        }
        self.stage = if self.info_len == 0 { Stage::Fcs } else { Stage::Info };
    }

    /// 把当前累积的内容作为垃圾丢弃，回到寻找标志的状态。
    fn discard(&mut self, events: &mut Vec<DecodeEvent>) {
        let mut raw = std::mem::take(&mut self.header);
        raw.append(&mut self.info);
        if !raw.is_empty() {
            self.flush_garbage(events);
            events.push(DecodeEvent::Garbage(raw));
        }
        self.stage = Stage::Hunting;
        self.info_len = 0;
    }

    /// 帧收全了，校验并产出事件。
    fn finish_frame(&mut self, events: &mut Vec<DecodeEvent>) {
        let header = std::mem::take(&mut self.header);
        let info = std::mem::take(&mut self.info);
        let received = self.fcs;

        let Some((ftype, pf)) = FrameType::from_control(header[1]) else {
            let mut raw = header;
            raw.extend_from_slice(&info);
            raw.push(received);
            events.push(DecodeEvent::Garbage(raw));
            return;
        };

        // 规范 5.2.7.1：UIH 只校验头部，其余帧型连 info 一起校验。
        let ok = if ftype == FrameType::Uih {
            fcs::check(&header, received)
        } else {
            let mut buf = header.clone();
            buf.extend_from_slice(&info);
            fcs::check(&buf, received)
        };

        if !ok {
            let mut raw = header;
            raw.extend_from_slice(&info);
            raw.push(received);
            events.push(DecodeEvent::FcsError { raw });
            return;
        }

        events.push(DecodeEvent::Frame(Frame {
            dlci: header[0] >> 2,
            cr: header[0] & 0x02 != 0,
            pf,
            ftype,
            info,
        }));
    }

    fn push_garbage(&mut self, b: u8, events: &mut Vec<DecodeEvent>) {
        self.garbage.push(b);
        if self.garbage.len() >= 256 {
            self.flush_garbage(events);
        }
    }

    fn flush_garbage(&mut self, events: &mut Vec<DecodeEvent>) {
        if !self.garbage.is_empty() {
            events.push(DecodeEvent::Garbage(std::mem::take(&mut self.garbage)));
        }
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
    fn frame_whose_fcs_equals_the_flag_byte_is_not_lost() {
        // 真实案例：Quectel EC800M 对 AT+CPIN? 的应答。info 长 31 字节，
        // 于是长度字节是 0x3F，头部 [07 EF 3F] 算出的 FCS 恰好等于标志字节 0xF9。
        // 若解码器靠「扫描到下一个 F9」定界，就会把 FCS 当成帧尾，整帧丢失。
        let info = b"AT+CPIN?\r\r\n+CPIN: READY\r\n\r\nOK\r\n".to_vec();
        assert_eq!(info.len(), 31, "用例前提：info 恰好 31 字节");
        let wire = Frame::uih(1, info.clone()).encode();
        assert_eq!(
            wire[wire.len() - 2],
            FLAG,
            "用例前提：这一帧的 FCS 恰好等于标志字节"
        );

        let mut d = FrameDecoder::new(1024);
        let got = frames(d.push(&wire));
        assert_eq!(got.len(), 1, "FCS 撞上标志字节的帧不能丢");
        assert_eq!(got[0].info, info);
    }

    #[test]
    fn info_containing_the_flag_byte_is_delimited_by_length() {
        // Basic 模式没有转义机制，0xF9 可以合法出现在 info 里，
        // 只能靠长度字段定界。二进制数据通道必须依赖这一点。
        let info = vec![0x01, 0xF9, 0x02, 0xF9, 0xF9, 0x03];
        let wire = Frame::uih(1, info.clone()).encode();
        let mut d = FrameDecoder::new(1024);
        let got = frames(d.push(&wire));
        assert_eq!(got.len(), 1, "info 含标志字节的帧不能丢");
        assert_eq!(got[0].info, info);
    }

    #[test]
    fn flag_heavy_frame_split_across_reads_still_decodes() {
        // 同上，但逐字节喂入，确保增量路径也按长度定界
        let info = vec![0xF9; 10];
        let wire = Frame::uih(2, info.clone()).encode();
        let mut d = FrameDecoder::new(1024);
        let mut got = Vec::new();
        for b in &wire {
            got.extend(frames(d.push(&[*b])));
        }
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].info, info);
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
