//! 串口的帧格式参数：数据位、校验、停止位。
//!
//! 用 `8N1` 这种串口工具通用记号表示，minicom/putty/screen 都是这个写法。
//! 本模块是纯逻辑，不碰 I/O，因此可以完全脱离硬件测试。

use serialport::{DataBits, Parity, StopBits};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameFormat {
    pub data_bits: DataBits,
    pub parity: Parity,
    pub stop_bits: StopBits,
}

impl Default for FrameFormat {
    fn default() -> Self {
        FrameFormat {
            data_bits: DataBits::Eight,
            parity: Parity::None,
            stop_bits: StopBits::One,
        }
    }
}

impl FromStr for FrameFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let chars: Vec<char> = s.chars().collect();
        if chars.len() != 3 {
            return Err(format!("帧格式必须是 3 个字符，如 8N1，收到 {s:?}"));
        }

        let data_bits = match chars[0] {
            '5' => DataBits::Five,
            '6' => DataBits::Six,
            '7' => DataBits::Seven,
            '8' => DataBits::Eight,
            c => return Err(format!("数据位必须是 5-8，收到 {c:?}")),
        };

        // 校验位大小写不敏感：8e1 与 8E1 等价。
        // 报错时回显 chars[1]（用户原样输入）而非大写化的结果，
        // 否则输入 8x1 会被告知「收到 'X'」，与他敲的不符。
        let parity = match chars[1].to_ascii_uppercase() {
            'N' => Parity::None,
            'E' => Parity::Even,
            'O' => Parity::Odd,
            _ => return Err(format!("校验位必须是 N/E/O，收到 {:?}", chars[1])),
        };

        let stop_bits = match chars[2] {
            '1' => StopBits::One,
            '2' => StopBits::Two,
            c => return Err(format!("停止位必须是 1 或 2，收到 {c:?}")),
        };

        Ok(FrameFormat { data_bits, parity, stop_bits })
    }
}

impl fmt::Display for FrameFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = match self.data_bits {
            DataBits::Five => '5',
            DataBits::Six => '6',
            DataBits::Seven => '7',
            DataBits::Eight => '8',
        };
        let p = match self.parity {
            Parity::None => 'N',
            Parity::Even => 'E',
            Parity::Odd => 'O',
        };
        let s = match self.stop_bits {
            StopBits::One => '1',
            StopBits::Two => '2',
        };
        write!(f, "{d}{p}{s}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_8n1() {
        let f = FrameFormat::default();
        assert_eq!(f.data_bits, DataBits::Eight);
        assert_eq!(f.parity, Parity::None);
        assert_eq!(f.stop_bits, StopBits::One);
        assert_eq!(f.to_string(), "8N1");
    }

    #[test]
    fn parses_common_formats() {
        let f: FrameFormat = "7E1".parse().unwrap();
        assert_eq!(f.data_bits, DataBits::Seven);
        assert_eq!(f.parity, Parity::Even);
        assert_eq!(f.stop_bits, StopBits::One);

        let f: FrameFormat = "8O2".parse().unwrap();
        assert_eq!(f.data_bits, DataBits::Eight);
        assert_eq!(f.parity, Parity::Odd);
        assert_eq!(f.stop_bits, StopBits::Two);

        let f: FrameFormat = "5N1".parse().unwrap();
        assert_eq!(f.data_bits, DataBits::Five);

        let f: FrameFormat = "6E2".parse().unwrap();
        assert_eq!(f.data_bits, DataBits::Six);
    }

    #[test]
    fn parity_letter_is_case_insensitive() {
        let lower: FrameFormat = "8e1".parse().unwrap();
        let upper: FrameFormat = "8E1".parse().unwrap();
        assert_eq!(lower, upper);
        // 显示统一为大写
        assert_eq!(lower.to_string(), "8E1");
    }

    #[test]
    fn rejects_bad_data_bits_naming_the_offender() {
        let e = "9N1".parse::<FrameFormat>().unwrap_err();
        assert!(e.contains("数据位"), "实际: {e}");
        assert!(e.contains('9'), "错误信息应指出是哪一位不合法，实际: {e}");
    }

    #[test]
    fn rejects_bad_parity_naming_the_offender() {
        let e = "8X1".parse::<FrameFormat>().unwrap_err();
        assert!(e.contains("校验位"), "实际: {e}");
        assert!(e.contains('X'), "错误信息应指出是哪一位不合法，实际: {e}");
    }

    #[test]
    fn bad_parity_error_echoes_what_the_user_typed() {
        // 小写输入报错时也要回显用户原样敲的字符，不能回显大写化之后的
        let e = "8x1".parse::<FrameFormat>().unwrap_err();
        assert!(e.contains('x'), "应回显用户输入的小写 x，实际: {e}");
    }

    #[test]
    fn rejects_bad_stop_bits_naming_the_offender() {
        let e = "8N3".parse::<FrameFormat>().unwrap_err();
        assert!(e.contains("停止位"), "实际: {e}");
        assert!(e.contains('3'), "错误信息应指出是哪一位不合法，实际: {e}");
    }

    #[test]
    fn rejects_wrong_length() {
        for bad in ["8N", "8N11", "", "8"] {
            let e = bad.parse::<FrameFormat>().unwrap_err();
            assert!(e.contains("3 个字符"), "输入 {bad:?} 的错误信息: {e}");
        }
    }

    #[test]
    fn display_and_parse_roundtrip_over_all_combinations() {
        let mut count = 0;
        for d in ['5', '6', '7', '8'] {
            for p in ['N', 'E', 'O'] {
                for s in ['1', '2'] {
                    let text: String = [d, p, s].iter().collect();
                    let parsed: FrameFormat = text.parse().unwrap();
                    assert_eq!(parsed.to_string(), text);
                    assert_eq!(text.parse::<FrameFormat>().unwrap(), parsed);
                    count += 1;
                }
            }
        }
        assert_eq!(count, 24, "应覆盖 4×3×2 种组合");
    }
}
