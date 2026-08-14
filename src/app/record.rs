//! 一条收发记录及其 hex/ASCII 呈现。

use chrono::{DateTime, Local};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Tx,
    Rx,
    /// 工具自身产生的提示信息。
    Info,
    /// 错误提示。
    Error,
}

impl Direction {
    pub fn label(self) -> &'static str {
        match self {
            Direction::Tx => "TX",
            Direction::Rx => "RX",
            Direction::Info => "--",
            Direction::Error => "!!",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Record {
    pub ts: DateTime<Local>,
    pub dir: Direction,
    pub dlci: u8,
    pub bytes: Vec<u8>,
    /// 附加说明，用于 Info/Error 记录。
    pub note: Option<String>,
}

impl Record {
    pub fn new(dir: Direction, dlci: u8, bytes: Vec<u8>) -> Record {
        Record { ts: Local::now(), dir, dlci, bytes, note: None }
    }

    /// 提示类记录：只有文字，没有字节。
    pub fn info(dlci: u8, text: impl Into<String>) -> Record {
        Record {
            ts: Local::now(),
            dir: Direction::Info,
            dlci,
            bytes: Vec::new(),
            note: Some(text.into()),
        }
    }

    /// 错误类记录。
    pub fn error(dlci: u8, text: impl Into<String>) -> Record {
        Record {
            ts: Local::now(),
            dir: Direction::Error,
            dlci,
            bytes: Vec::new(),
            note: Some(text.into()),
        }
    }

    /// 记录头行，形如 `19:04:12.331 TX 8B`；带 note 时改为显示 note。
    pub fn header(&self) -> String {
        let ts = self.ts.format("%H:%M:%S%.3f");
        match &self.note {
            Some(n) => format!("{ts} {} {n}", self.dir.label()),
            None => format!("{ts} {} {}B", self.dir.label(), self.bytes.len()),
        }
    }
}

/// 把字节切成 (hex 列, ascii 列) 行对。
///
/// hex 列以空格补齐到定宽，保证多行之间 ASCII 列对齐。
pub fn hexdump_lines(bytes: &[u8], per_line: usize) -> Vec<(String, String)> {
    let width = per_line * 3 - 1;
    bytes
        .chunks(per_line)
        .map(|chunk| {
            let hex = chunk
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(" ");
            let ascii = chunk
                .iter()
                .map(|&b| if (0x20..=0x7E).contains(&b) { b as char } else { '.' })
                .collect::<String>();
            (format!("{hex:<width$}"), ascii)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hexdump_pads_short_lines_so_ascii_column_aligns() {
        let lines = hexdump_lines(b"AT\r", 8);
        assert_eq!(lines.len(), 1);
        let (hex, ascii) = &lines[0];
        // 8 字节 × 3 字符 - 1 = 23 宽
        assert_eq!(hex.len(), 23, "hex 列必须定宽，实际: {hex:?}");
        assert_eq!(hex.trim_end(), "41 54 0D");
        assert_eq!(ascii.as_str(), "AT.");
    }

    #[test]
    fn hexdump_splits_by_bytes_per_line() {
        let data: Vec<u8> = (0..20).collect();
        let lines = hexdump_lines(&data, 8);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].0, "00 01 02 03 04 05 06 07");
        assert_eq!(lines[2].0.trim_end(), "10 11 12 13");
    }

    #[test]
    fn non_printable_bytes_render_as_dot() {
        let lines = hexdump_lines(&[0x00, 0x1F, 0x20, 0x7E, 0x7F, 0xFF], 8);
        // 0x00/0x1F 不可打印 → ".."，0x20 是空格，0x7E 是 '~'，0x7F/0xFF → ".."
        assert_eq!(lines[0].1, ".. ~..", "只有 0x20..=0x7E 可打印");
    }

    #[test]
    fn empty_input_produces_no_lines() {
        assert!(hexdump_lines(&[], 8).is_empty());
    }

    #[test]
    fn sixteen_byte_width_is_supported() {
        let data: Vec<u8> = (0..16).collect();
        let lines = hexdump_lines(&data, 16);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0.len(), 47); // 16*3-1
    }

    #[test]
    fn record_header_shows_time_direction_and_size() {
        let r = Record::new(Direction::Tx, 1, b"AT\r".to_vec());
        let h = r.header();
        assert!(h.ends_with("TX 3B"), "实际: {h}");
        // HH:MM:SS.mmm 共 12 字符，加空格与方向
        assert_eq!(h.len(), 12 + 1 + 5, "实际: {h:?}");
    }

    #[test]
    fn error_record_header_carries_note() {
        let mut r = Record::new(Direction::Error, 1, Vec::new());
        r.note = Some("写失败".into());
        assert!(r.header().contains("写失败"));
    }

    #[test]
    fn direction_labels_are_stable() {
        assert_eq!(Direction::Tx.label(), "TX");
        assert_eq!(Direction::Rx.label(), "RX");
        assert_eq!(Direction::Info.label(), "--");
        assert_eq!(Direction::Error.label(), "!!");
    }
}
