//! Basic 模式（3GPP TS 27.010 第 5.2.1 节）帧的结构与编码。

use crate::mux::fcs;

/// 帧起止标志。
pub const FLAG: u8 = 0xF9;

/// 帧类型。控制字节中 P/F 位（0x10）已剥离。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Sabm,
    Ua,
    Dm,
    Disc,
    Uih,
}

impl FrameType {
    /// 组装控制字节，`pf` 为 true 时置 P/F 位。
    pub fn control_byte(self, pf: bool) -> u8 {
        let base = match self {
            FrameType::Sabm => 0x2F,
            FrameType::Ua => 0x63,
            FrameType::Dm => 0x0F,
            FrameType::Disc => 0x43,
            FrameType::Uih => 0xEF,
        };
        if pf { base | 0x10 } else { base & !0x10 }
    }

    /// 从控制字节解析出类型与 P/F 位。
    pub fn from_control(c: u8) -> Option<(FrameType, bool)> {
        let pf = c & 0x10 != 0;
        let ft = match c & !0x10 {
            0x2F => FrameType::Sabm,
            0x63 => FrameType::Ua,
            0x0F => FrameType::Dm,
            0x43 => FrameType::Disc,
            0xEF => FrameType::Uih,
            _ => return None,
        };
        Some((ft, pf))
    }

    /// 人类可读的短名，用于原始帧视图。
    pub fn name(self) -> &'static str {
        match self {
            FrameType::Sabm => "SABM",
            FrameType::Ua => "UA",
            FrameType::Dm => "DM",
            FrameType::Disc => "DISC",
            FrameType::Uih => "UIH",
        }
    }
}

/// 一个 Basic 模式帧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub dlci: u8,
    /// Command/Response 位。本工具是 initiator，发命令时为 true。
    pub cr: bool,
    /// Poll/Final 位。
    pub pf: bool,
    pub ftype: FrameType,
    pub info: Vec<u8>,
}

impl Frame {
    pub fn sabm(dlci: u8) -> Frame {
        Frame { dlci, cr: true, pf: true, ftype: FrameType::Sabm, info: Vec::new() }
    }

    pub fn ua(dlci: u8) -> Frame {
        Frame { dlci, cr: true, pf: true, ftype: FrameType::Ua, info: Vec::new() }
    }

    pub fn disc(dlci: u8) -> Frame {
        Frame { dlci, cr: true, pf: true, ftype: FrameType::Disc, info: Vec::new() }
    }

    pub fn uih(dlci: u8, info: Vec<u8>) -> Frame {
        Frame { dlci, cr: true, pf: false, ftype: FrameType::Uih, info }
    }

    /// 地址字节：DLCI<<2 | C/R<<1 | EA。
    pub fn address_byte(&self) -> u8 {
        (self.dlci << 2) | (if self.cr { 0x02 } else { 0x00 }) | 0x01
    }

    /// 规范 5.2.7.1：UIH 帧的 FCS 只覆盖头部，其余帧型覆盖到 Info。
    fn fcs_covers_info(&self) -> bool {
        self.ftype != FrameType::Uih
    }

    /// 编码为线上字节，含首尾标志。
    pub fn encode(&self) -> Vec<u8> {
        let len = self.info.len();
        let mut header = Vec::with_capacity(4);
        header.push(self.address_byte());
        header.push(self.ftype.control_byte(self.pf));
        if len <= 127 {
            header.push(((len as u8) << 1) | 0x01);
        } else {
            header.push(((len as u8) & 0x7F) << 1); // EA=0
            header.push((len >> 7) as u8);
        }

        let checksum = if self.fcs_covers_info() {
            let mut buf = header.clone();
            buf.extend_from_slice(&self.info);
            fcs::fcs(&buf)
        } else {
            fcs::fcs(&header)
        };

        let mut out = Vec::with_capacity(header.len() + len + 3);
        out.push(FLAG);
        out.extend_from_slice(&header);
        out.extend_from_slice(&self.info);
        out.push(checksum);
        out.push(FLAG);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sabm_dlci0_encodes_to_spec_bytes() {
        // 规范帧：F9 03 3F 01 1C F9
        assert_eq!(
            Frame::sabm(0).encode(),
            vec![0xF9, 0x03, 0x3F, 0x01, 0x1C, 0xF9]
        );
    }

    #[test]
    fn ua_dlci0_encodes_to_spec_bytes() {
        // 规范帧：F9 03 73 01 D7 F9
        // address 是 0x03 而非 0x01：按 27.010 表 1，responder 发出的响应 C/R 也为 1。
        assert_eq!(
            Frame::ua(0).encode(),
            vec![0xF9, 0x03, 0x73, 0x01, 0xD7, 0xF9]
        );
    }

    #[test]
    fn dlci_lands_in_high_six_bits_of_address() {
        let bytes = Frame::sabm(3).encode();
        // address = DLCI<<2 | CR<<1 | EA = 3<<2 | 1<<1 | 1 = 0x0F
        assert_eq!(bytes[1], 0x0F);
    }

    #[test]
    fn short_uih_uses_single_length_byte() {
        let bytes = Frame::uih(1, b"AT\r".to_vec()).encode();
        // F9 | addr 07 | ctrl EF | len (3<<1|1)=07 | AT\r | fcs | F9
        assert_eq!(&bytes[..4], &[0xF9, 0x07, 0xEF, 0x07]);
        assert_eq!(&bytes[4..7], b"AT\r");
        assert_eq!(bytes.len(), 9);
        assert_eq!(*bytes.last().unwrap(), 0xF9);
    }

    #[test]
    fn long_uih_uses_two_length_bytes() {
        let info = vec![0xAAu8; 200];
        let bytes = Frame::uih(1, info).encode();
        // len=200 → L1=(200&0x7F)<<1=0x90 (EA=0), L2=200>>7=0x01
        assert_eq!(bytes[3], 0x90);
        assert_eq!(bytes[4], 0x01);
        // F9 + addr + ctrl + 2 长度 + 200 信息 + fcs + F9
        assert_eq!(bytes.len(), 207);
    }

    #[test]
    fn uih_fcs_covers_header_only() {
        let f = Frame::uih(1, b"AT\r".to_vec());
        let bytes = f.encode();
        let expected = crate::mux::fcs::fcs(&[0x07, 0xEF, 0x07]);
        assert_eq!(bytes[bytes.len() - 2], expected);
    }

    #[test]
    fn non_uih_fcs_covers_info_too() {
        // 带 info 的 SABM 极少见，但规范要求 FCS 覆盖 info。
        // 这里断言两种算法结果不同，从而锁住分支行为。
        // 注意 info 不能取 [0x11,0x22]：该向量下两种算法恰好都得出 0xD9，
        // 测试会退化成永远通过。下面的 assert_ne! 就是防止这种退化。
        let mut f = Frame::sabm(1);
        f.info = vec![0xAA, 0xBB];
        let bytes = f.encode();
        let header = [0x07u8, 0x3F, (2 << 1) | 1];
        let with_info = crate::mux::fcs::fcs(&[0x07, 0x3F, 0x05, 0xAA, 0xBB]);
        let header_only = crate::mux::fcs::fcs(&header);
        assert_ne!(with_info, header_only, "测试向量必须能区分两种算法");
        assert_eq!(bytes[bytes.len() - 2], with_info);
    }

    #[test]
    fn control_byte_roundtrip() {
        for ft in [
            FrameType::Sabm,
            FrameType::Ua,
            FrameType::Dm,
            FrameType::Disc,
            FrameType::Uih,
        ] {
            for pf in [true, false] {
                let c = ft.control_byte(pf);
                assert_eq!(FrameType::from_control(c), Some((ft, pf)));
            }
        }
    }

    #[test]
    fn unknown_control_byte_is_rejected() {
        assert_eq!(FrameType::from_control(0x00), None);
    }
}
