//! 底部输入框的状态与内容解析。

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMode {
    /// 默认模式：直接敲 AT 命令是最常见的用法。
    #[default]
    Ascii,
    Hex,
}

impl InputMode {
    pub fn label(self) -> &'static str {
        match self {
            InputMode::Ascii => "ASCII",
            InputMode::Hex => "HEX",
        }
    }
}

/// 把输入框文本解析成待发送字节。
///
/// `append_cr` 只在 ASCII 模式生效；HEX 模式下用户完全掌控每个字节。
pub fn parse_input(text: &str, mode: InputMode, append_cr: bool) -> Result<Vec<u8>, String> {
    match mode {
        InputMode::Ascii => {
            if text.is_empty() {
                return Err("输入为空".to_string());
            }
            let mut out = text.as_bytes().to_vec();
            if append_cr {
                out.push(b'\r');
            }
            Ok(out)
        }
        InputMode::Hex => {
            let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            if compact.is_empty() {
                return Err("输入为空".to_string());
            }
            if let Some(bad) = compact.chars().find(|c| !c.is_ascii_hexdigit()) {
                return Err(format!("非法十六进制字符 {bad:?}"));
            }
            if !compact.len().is_multiple_of(2) {
                return Err("十六进制位数必须为偶数".to_string());
            }
            let bytes = compact
                .as_bytes()
                .chunks(2)
                .map(|pair| {
                    let s = std::str::from_utf8(pair).expect("已校验为 ASCII");
                    u8::from_str_radix(s, 16).expect("已校验为十六进制")
                })
                .collect();
            Ok(bytes)
        }
    }
}

/// 输入框状态。
#[derive(Debug, Default)]
pub struct InputBox {
    pub buffer: String,
    pub mode: InputMode,
    pub history: Vec<String>,
    /// 历史浏览位置。None 表示正在编辑新内容。
    cursor: Option<usize>,
}

impl InputBox {
    pub fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            InputMode::Ascii => InputMode::Hex,
            InputMode::Hex => InputMode::Ascii,
        };
    }

    pub fn insert(&mut self, c: char) {
        self.buffer.push(c);
        self.cursor = None;
    }

    pub fn backspace(&mut self) {
        self.buffer.pop();
        self.cursor = None;
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = None;
    }

    /// 提交当前内容：入历史、清空缓冲、返回文本。
    pub fn submit(&mut self) -> String {
        let text = std::mem::take(&mut self.buffer);
        if !text.is_empty() && self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        self.cursor = None;
        text
    }

    /// 向更早的历史移动。
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.cursor {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.cursor = Some(next);
        self.buffer = self.history[next].clone();
    }

    /// 向更新的历史移动，越过末尾则回到空输入。
    pub fn history_next(&mut self) {
        let Some(i) = self.cursor else { return };
        if i + 1 >= self.history.len() {
            self.cursor = None;
            self.buffer.clear();
        } else {
            self.cursor = Some(i + 1);
            self.buffer = self.history[i + 1].clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_mode_sends_literal_text() {
        assert_eq!(parse_input("AT", InputMode::Ascii, false).unwrap(), b"AT");
    }

    #[test]
    fn ascii_mode_appends_cr_when_asked() {
        assert_eq!(parse_input("AT", InputMode::Ascii, true).unwrap(), b"AT\r");
    }

    #[test]
    fn hex_mode_accepts_spaced_bytes() {
        assert_eq!(
            parse_input("41 54 0D", InputMode::Hex, false).unwrap(),
            vec![0x41, 0x54, 0x0D]
        );
    }

    #[test]
    fn hex_mode_accepts_run_together_bytes() {
        assert_eq!(
            parse_input("41540D", InputMode::Hex, false).unwrap(),
            vec![0x41, 0x54, 0x0D]
        );
    }

    #[test]
    fn hex_mode_ignores_append_cr() {
        // HEX 模式下用户完全掌控字节，不能偷偷加 \r
        assert_eq!(parse_input("41", InputMode::Hex, true).unwrap(), vec![0x41]);
    }

    #[test]
    fn hex_mode_rejects_odd_digit_count() {
        let err = parse_input("415", InputMode::Hex, false).unwrap_err();
        assert!(err.contains("偶数"), "实际: {err}");
    }

    #[test]
    fn hex_mode_rejects_non_hex_characters() {
        let err = parse_input("4G", InputMode::Hex, false).unwrap_err();
        assert!(err.contains("非法"), "实际: {err}");
    }

    #[test]
    fn empty_input_is_rejected_in_both_modes() {
        assert!(parse_input("", InputMode::Ascii, true).is_err());
        assert!(parse_input("   ", InputMode::Hex, false).is_err());
    }

    #[test]
    fn mode_toggles_between_ascii_and_hex() {
        let mut b = InputBox::default();
        assert_eq!(b.mode, InputMode::Ascii);
        b.toggle_mode();
        assert_eq!(b.mode, InputMode::Hex);
        b.toggle_mode();
        assert_eq!(b.mode, InputMode::Ascii);
    }

    #[test]
    fn editing_inserts_and_deletes_at_cursor() {
        let mut b = InputBox::default();
        b.insert('A');
        b.insert('T');
        assert_eq!(b.buffer, "AT");
        b.backspace();
        assert_eq!(b.buffer, "A");
        b.clear();
        assert_eq!(b.buffer, "");
    }

    #[test]
    fn submit_pushes_history_and_clears_buffer() {
        let mut b = InputBox::default();
        b.insert('A');
        let text = b.submit();
        assert_eq!(text, "A");
        assert_eq!(b.buffer, "");
        assert_eq!(b.history, vec!["A".to_string()]);
    }

    #[test]
    fn history_navigation_walks_backwards_then_forwards() {
        let mut b = InputBox::default();
        for c in "one".chars() {
            b.insert(c);
        }
        b.submit();
        for c in "two".chars() {
            b.insert(c);
        }
        b.submit();

        b.history_prev();
        assert_eq!(b.buffer, "two");
        b.history_prev();
        assert_eq!(b.buffer, "one");
        b.history_next();
        assert_eq!(b.buffer, "two");
        b.history_next();
        assert_eq!(b.buffer, "", "走到最新之后应回到空输入");
    }

    #[test]
    fn duplicate_consecutive_history_entries_are_collapsed() {
        let mut b = InputBox::default();
        for c in "x".chars() {
            b.insert(c);
        }
        b.submit();
        for c in "x".chars() {
            b.insert(c);
        }
        b.submit();
        assert_eq!(b.history.len(), 1);
    }
}
