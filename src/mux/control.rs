//! DLCI 0 上承载的多路复用控制通道消息（规范第 5.4.6 节）。
//!
//! 消息格式：type 字节 (含 EA/CR 位) + 长度字节 (EA 编码) + 载荷。
//! 本工具只实现调试实际需要的 MSC 与 CLD，其余类型原样保留以便显示。

/// 控制消息类型位（已去掉 EA 与 C/R）。
const TYPE_NSC: u8 = 0x08;
const TYPE_MSC: u8 = 0xE0;
const TYPE_CLD: u8 = 0xC0;

/// 已知控制消息类型的名字。
///
/// 取值是规范里的类型位去掉 EA 与 C/R 之后的结果，与 Linux `n_gsm`
/// 的 `CMD_*` 常量一致（那些常量带 EA=1、C/R=0，例如 MSC 是 0xE1）。
/// 本工具只实现 MSC / CLD / NSC 三种，其余仅用于把类型名显示出来——
/// 排查时知道对端发的是 PN 还是 FCoff，比看到一句「未知」有用得多。
fn type_name(ctype: u8) -> Option<&'static str> {
    match ctype {
        TYPE_NSC => Some("NSC"),
        0x10 => Some("Test"),
        0x20 => Some("PSC"),
        0x28 => Some("RLS"),
        0x30 => Some("FCoff"),
        0x40 => Some("PN"),
        0x48 => Some("RPN"),
        0x50 => Some("FCon"),
        0x68 => Some("SNC"),
        TYPE_CLD => Some("CLD"),
        TYPE_MSC => Some("MSC"),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlMessage {
    /// 调制解调器状态命令。`signals` 为 V.24 信号字节。
    Msc { dlci: u8, signals: u8 },
    /// 关闭整个复用。
    Cld,
    /// 对端回复「不支持该命令」，载荷是被拒绝的命令类型字节。
    ///
    /// 实测 Quectel EC800M 就用它拒绝 MSC。
    Nsc { rejected: u8 },
    /// 未实现的类型，保留原始内容用于展示。
    Unknown { ctype: u8, data: Vec<u8> },
}

impl ControlMessage {
    /// 编码为命令（C/R 位置 1）。
    pub fn encode_command(&self) -> Vec<u8> {
        let (ctype, payload) = match self {
            ControlMessage::Msc { dlci, signals } => {
                (TYPE_MSC, vec![(dlci << 2) | 0x03, *signals])
            }
            ControlMessage::Cld => (TYPE_CLD, Vec::new()),
            ControlMessage::Nsc { rejected } => (TYPE_NSC, vec![*rejected]),
            ControlMessage::Unknown { ctype, data } => (*ctype, data.clone()),
        };
        let mut out = Vec::with_capacity(payload.len() + 2);
        out.push(ctype | 0x02 | 0x01); // C/R=1, EA=1
        out.push(((payload.len() as u8) << 1) | 0x01);
        out.extend_from_slice(&payload);
        out
    }

    /// 解析一条控制消息，返回消息本身与「是否为命令」。
    pub fn parse(info: &[u8]) -> Option<(ControlMessage, bool)> {
        if info.len() < 2 {
            return None;
        }
        let type_byte = info[0];
        let is_command = type_byte & 0x02 != 0;
        let ctype = type_byte & !0x03;

        let len = (info[1] >> 1) as usize;
        let payload = info.get(2..2 + len)?;

        let msg = match ctype {
            TYPE_MSC if payload.len() >= 2 => ControlMessage::Msc {
                dlci: payload[0] >> 2,
                signals: payload[1],
            },
            TYPE_CLD => ControlMessage::Cld,
            TYPE_NSC if !payload.is_empty() => ControlMessage::Nsc { rejected: payload[0] },
            _ => ControlMessage::Unknown { ctype, data: payload.to_vec() },
        };
        Some((msg, is_command))
    }

    /// 界面展示用的中文描述。
    pub fn describe(&self) -> String {
        match self {
            ControlMessage::Msc { dlci, signals } => {
                format!("MSC DLCI {dlci} 信号 {signals:#04X}")
            }
            ControlMessage::Cld => "CLD 关闭复用".to_string(),
            ControlMessage::Nsc { rejected } => {
                // 把被拒绝的类型翻译成名字，否则只看到一个裸字节
                match type_name(rejected & !0x03) {
                    Some(name) => format!("NSC 对端不支持 {name}"),
                    None => format!("NSC 对端不支持 type={rejected:#04X}"),
                }
            }
            ControlMessage::Unknown { ctype, data } => match type_name(*ctype) {
                Some(name) => format!("{name}（本工具未解析）{} 字节", data.len()),
                None => format!("未知控制消息 type={ctype:#04X} {} 字节", data.len()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cld_command_encodes_to_two_bytes() {
        // type=CLD 命令 (0xC0|CR|EA = 0xC3)，长度 0 (EA=1 → 0x01)
        assert_eq!(ControlMessage::Cld.encode_command(), vec![0xC3, 0x01]);
    }

    #[test]
    fn msc_command_encodes_dlci_and_signals() {
        let m = ControlMessage::Msc { dlci: 1, signals: 0x8D };
        // type=0xE3, len=2 → 0x05, addr=(1<<2)|3=0x07, signal=0x8D
        assert_eq!(m.encode_command(), vec![0xE3, 0x05, 0x07, 0x8D]);
    }

    #[test]
    fn parses_cld_response() {
        // 响应把 CR 位清零：0xC1
        let (msg, is_command) = ControlMessage::parse(&[0xC1, 0x01]).unwrap();
        assert_eq!(msg, ControlMessage::Cld);
        assert!(!is_command);
    }

    #[test]
    fn parses_msc_command() {
        let (msg, is_command) = ControlMessage::parse(&[0xE3, 0x05, 0x07, 0x8D]).unwrap();
        assert_eq!(msg, ControlMessage::Msc { dlci: 1, signals: 0x8D });
        assert!(is_command);
    }

    #[test]
    fn parses_nsc_rejection_captured_from_real_hardware() {
        // 实测 Quectel EC800M 对我们的 MSC 的回复，控制通道 info 是 09 03 E3：
        // type=0x09(NSC, 响应), 长度 1, 载荷 0xE3 即被拒绝的 MSC 命令类型。
        let (msg, is_command) = ControlMessage::parse(&[0x09, 0x03, 0xE3]).unwrap();
        assert_eq!(msg, ControlMessage::Nsc { rejected: 0xE3 });
        assert!(!is_command, "NSC 是响应而非命令");

        let d = msg.describe();
        assert!(d.contains("不支持"), "实际: {d}");
        assert!(d.contains("MSC"), "应指明被拒绝的是哪条命令，实际: {d}");
    }

    #[test]
    fn known_but_unimplemented_types_show_their_name() {
        // 没实现解析也该说清是什么，"未知" 对排查毫无帮助
        let (msg, _) = ControlMessage::parse(&[0x41, 0x03, 0x07]).unwrap();
        assert!(msg.describe().contains("PN"), "实际: {}", msg.describe());

        let (msg, _) = ControlMessage::parse(&[0x31, 0x01]).unwrap();
        assert!(msg.describe().contains("FCoff"), "实际: {}", msg.describe());
    }

    #[test]
    fn unknown_type_is_preserved_for_display() {
        let (msg, _) = ControlMessage::parse(&[0x83, 0x03, 0xAA]).unwrap();
        match msg {
            ControlMessage::Unknown { ctype, data } => {
                assert_eq!(ctype, 0x80);
                assert_eq!(data, vec![0xAA]);
            }
            other => panic!("期望 Unknown，得到 {other:?}"),
        }
    }

    #[test]
    fn truncated_message_returns_none() {
        assert!(ControlMessage::parse(&[]).is_none());
        assert!(ControlMessage::parse(&[0xE3]).is_none());
        // 声明长度 2 但只有 1 字节数据
        assert!(ControlMessage::parse(&[0xE3, 0x05, 0x07]).is_none());
    }

    #[test]
    fn command_roundtrips_through_parse() {
        let m = ControlMessage::Msc { dlci: 5, signals: 0x8D };
        let (back, is_command) = ControlMessage::parse(&m.encode_command()).unwrap();
        assert_eq!(back, m);
        assert!(is_command);
    }

    #[test]
    fn describe_is_human_readable() {
        assert_eq!(ControlMessage::Cld.describe(), "CLD 关闭复用");
        assert!(
            ControlMessage::Msc { dlci: 1, signals: 0x8D }
                .describe()
                .contains("MSC")
        );
    }
}
