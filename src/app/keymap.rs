//! 按键到命令的翻译。
//!
//! 本模块只修改「纯界面」状态（输入框、焦点、滚动、弹窗），
//! 凡是需要 I/O 的动作都以 `Command` 返回，由主循环执行。
//! 这样所有快捷键都能脱离终端测试。
//!
//! 快捷键选择上有一处不能改：**输入模式切换用 Ctrl+T 而非 Ctrl+H**。
//! 终端里 Ctrl+H 发送的是 0x08，crossterm 会把它解析成 Backspace，
//! 既切不了模式又和退格冲突。同理 Ctrl+I/M/J 分别与 Tab/Enter 冲突，未使用。

use crate::app::input::parse_input;
use crate::app::state::{AppState, CONTROL_ITEMS, Dialog};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// 需要主循环执行的动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    None,
    Quit,
    /// 向指定 DLCI 发送数据。
    Send { dlci: u8, bytes: Vec<u8> },
    /// 对指定 DLCI 建链，并绑定到槽位。
    OpenDlc { dlci: u8, slot: usize },
    /// 关闭指定 DLCI。
    CloseDlc { dlci: u8 },
    /// 在指定 DLCI 上发 MSC。
    SendMsc { dlci: u8 },
    /// 关闭整个复用。
    CloseMux,
    /// 开关会话日志。
    ToggleLog,
}

/// 处理一次按键。会就地更新界面状态，并返回需要执行的命令。
pub fn map_key(state: &mut AppState, key: KeyEvent) -> Command {
    // 每次按键先清掉上一条提示，避免旧错误滞留。
    state.notice = None;

    if state.dialog.is_some() {
        return dialog_key(state, key);
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        KeyCode::Char('q') if ctrl => Command::Quit,
        KeyCode::Char('t') if ctrl => {
            state.input.toggle_mode();
            Command::None
        }
        KeyCode::Char('r') if ctrl => {
            state.show_raw = !state.show_raw;
            Command::None
        }
        KeyCode::Char('l') if ctrl => {
            state.clear_focused();
            Command::None
        }
        KeyCode::Char('s') if ctrl => Command::ToggleLog,
        KeyCode::Char('o') if ctrl => {
            state.dialog = Some(Dialog::Bind { slot: state.focus, input: String::new() });
            Command::None
        }
        KeyCode::Char('k') if ctrl => {
            state.dialog = Some(Dialog::Control { selected: 0 });
            Command::None
        }
        // 用 Ctrl+G 而不是惯例的 F1：F1/F2 已经用于切换通道槽位。
        KeyCode::Char('g') if ctrl => {
            state.dialog = Some(Dialog::Help);
            Command::None
        }
        KeyCode::Char(c) if !ctrl => {
            state.input.insert(c);
            Command::None
        }
        KeyCode::Backspace => {
            state.input.backspace();
            Command::None
        }
        KeyCode::Tab => {
            state.focus_next();
            Command::None
        }
        KeyCode::F(n) if (1..=2).contains(&n) => {
            state.set_focus((n - 1) as usize);
            Command::None
        }
        KeyCode::Up => {
            state.input.history_prev();
            Command::None
        }
        KeyCode::Down => {
            state.input.history_next();
            Command::None
        }
        KeyCode::PageUp => {
            state.scroll_focused(-5);
            Command::None
        }
        KeyCode::PageDown => {
            state.scroll_focused(5);
            Command::None
        }
        KeyCode::End => {
            state.follow_focused();
            Command::None
        }
        KeyCode::Enter => submit(state),
        _ => Command::None,
    }
}

/// 处理输入框回车。
fn submit(state: &mut AppState) -> Command {
    let Some(dlci) = state.target_dlci() else {
        state.notice = Some("当前槽位未绑定通道，按 Ctrl+O 绑定".into());
        return Command::None;
    };
    let mode = state.input.mode;
    let ending = state.cfg.line_ending;
    match parse_input(&state.input.buffer, mode, ending) {
        Ok(bytes) => {
            state.input.submit();
            Command::Send { dlci, bytes }
        }
        Err(e) => {
            state.notice = Some(e);
            Command::None
        }
    }
}

/// 弹窗打开时的按键处理。
fn dialog_key(state: &mut AppState, key: KeyEvent) -> Command {
    let Some(dialog) = state.dialog.clone() else {
        return Command::None;
    };

    if key.code == KeyCode::Esc {
        state.dialog = None;
        return Command::None;
    }

    match dialog {
        // 帮助页任意键关闭，不必记 Esc。
        Dialog::Help => {
            state.dialog = None;
            Command::None
        }
        Dialog::Bind { slot, mut input } => match key.code {
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if input.len() < 2 {
                    input.push(c);
                }
                state.dialog = Some(Dialog::Bind { slot, input });
                Command::None
            }
            KeyCode::Backspace => {
                input.pop();
                state.dialog = Some(Dialog::Bind { slot, input });
                Command::None
            }
            KeyCode::Enter => match input.parse::<u8>() {
                Ok(dlci) if (1..=63).contains(&dlci) => {
                    state.dialog = None;
                    Command::OpenDlc { dlci, slot }
                }
                _ => {
                    state.notice = Some("DLCI 必须在 1-63 之间".into());
                    Command::None
                }
            },
            _ => Command::None,
        },
        Dialog::Control { selected } => match key.code {
            KeyCode::Up => {
                let n = selected.saturating_sub(1);
                state.dialog = Some(Dialog::Control { selected: n });
                Command::None
            }
            KeyCode::Down => {
                let n = (selected + 1).min(CONTROL_ITEMS.len() - 1);
                state.dialog = Some(Dialog::Control { selected: n });
                Command::None
            }
            KeyCode::Enter => {
                state.dialog = None;
                let dlci = state.target_dlci();
                match selected {
                    0 => match dlci {
                        Some(d) => Command::SendMsc { dlci: d },
                        None => {
                            state.notice = Some("当前槽位未绑定通道".into());
                            Command::None
                        }
                    },
                    1 => match dlci {
                        Some(d) => Command::CloseDlc { dlci: d },
                        None => {
                            state.notice = Some("当前槽位未绑定通道".into());
                            Command::None
                        }
                    },
                    _ => Command::CloseMux,
                }
            }
            _ => Command::None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::{AppConfig, AppState, Dialog};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }
    fn code(k: KeyCode) -> KeyEvent {
        KeyEvent::new(k, KeyModifiers::NONE)
    }

    fn ready() -> AppState {
        let mut s = AppState::new(AppConfig::default());
        s.bind(1);
        s.bind(3);
        s
    }

    #[test]
    fn ctrl_q_quits() {
        let mut s = ready();
        assert!(matches!(map_key(&mut s, ctrl('q')), Command::Quit));
    }

    #[test]
    fn typing_appends_to_input_buffer() {
        let mut s = ready();
        map_key(&mut s, key('A'));
        map_key(&mut s, key('T'));
        assert_eq!(s.input.buffer, "AT");
    }

    #[test]
    fn enter_emits_send_with_parsed_bytes() {
        let mut s = ready();
        for c in "AT".chars() {
            map_key(&mut s, key(c));
        }
        match map_key(&mut s, code(KeyCode::Enter)) {
            Command::Send { dlci, bytes } => {
                assert_eq!(dlci, 1);
                assert_eq!(bytes, b"AT\r", "ASCII 模式默认追加 \\r");
            }
            other => panic!("期望 Send，得到 {other:?}"),
        }
        assert_eq!(s.input.buffer, "", "发送后应清空输入");
    }

    #[test]
    fn enter_with_bad_hex_sets_notice_and_sends_nothing() {
        let mut s = ready();
        map_key(&mut s, ctrl('t')); // 切到 HEX
        for c in "4G".chars() {
            map_key(&mut s, key(c));
        }
        assert!(matches!(map_key(&mut s, code(KeyCode::Enter)), Command::None));
        assert!(
            s.notice.as_ref().unwrap().contains("非法"),
            "实际: {:?}",
            s.notice
        );
        assert_eq!(s.input.buffer, "4G", "解析失败不应清空输入");
    }

    #[test]
    fn enter_without_bound_channel_warns() {
        let mut s = AppState::new(AppConfig::default());
        map_key(&mut s, key('A'));
        assert!(matches!(map_key(&mut s, code(KeyCode::Enter)), Command::None));
        assert!(s.notice.is_some());
    }

    #[test]
    fn function_keys_switch_focus() {
        let mut s = ready();
        map_key(&mut s, code(KeyCode::F(2)));
        assert_eq!(s.focus, 1);
        map_key(&mut s, code(KeyCode::F(1)));
        assert_eq!(s.focus, 0);
        map_key(&mut s, code(KeyCode::Tab));
        assert_eq!(s.focus, 1);
    }

    #[test]
    fn ctrl_t_toggles_input_mode() {
        use crate::app::input::InputMode;
        let mut s = ready();
        map_key(&mut s, ctrl('t'));
        assert_eq!(s.input.mode, InputMode::Hex);
    }

    #[test]
    fn ctrl_r_toggles_raw_view() {
        let mut s = ready();
        map_key(&mut s, ctrl('r'));
        assert!(s.show_raw);
        map_key(&mut s, ctrl('r'));
        assert!(!s.show_raw);
    }

    #[test]
    fn ctrl_l_clears_focused_channel() {
        use crate::app::record::{Direction, Record};
        let mut s = ready();
        s.push_record(Record::new(Direction::Rx, 1, vec![1]));
        map_key(&mut s, ctrl('l'));
        assert!(s.slots[0].records.is_empty());
    }

    #[test]
    fn page_keys_scroll_and_end_follows() {
        let mut s = ready();
        use crate::app::record::Record;
        for i in 0..30 {
            s.push_record(Record::info(1, format!("行{i}")));
        }
        map_key(&mut s, code(KeyCode::PageUp));
        assert!(!s.slots[0].follow);
        map_key(&mut s, code(KeyCode::End));
        assert!(s.slots[0].follow);
    }

    #[test]
    fn ctrl_o_opens_bind_dialog_for_focused_slot() {
        let mut s = ready();
        s.focus = 1;
        map_key(&mut s, ctrl('o'));
        assert_eq!(s.dialog, Some(Dialog::Bind { slot: 1, input: String::new() }));
    }

    #[test]
    fn bind_dialog_accepts_digits_and_emits_open() {
        let mut s = ready();
        s.unbind(1);
        s.focus = 0;
        map_key(&mut s, ctrl('o'));
        map_key(&mut s, key('7'));
        assert_eq!(s.dialog, Some(Dialog::Bind { slot: 0, input: "7".into() }));

        match map_key(&mut s, code(KeyCode::Enter)) {
            Command::OpenDlc { dlci, slot } => {
                assert_eq!(dlci, 7);
                assert_eq!(slot, 0);
            }
            other => panic!("期望 OpenDlc，得到 {other:?}"),
        }
        assert!(s.dialog.is_none(), "确认后应关闭弹窗");
    }

    #[test]
    fn bind_dialog_rejects_out_of_range_dlci() {
        let mut s = ready();
        s.unbind(1);
        map_key(&mut s, ctrl('o'));
        for c in "64".chars() {
            map_key(&mut s, key(c));
        }
        assert!(matches!(map_key(&mut s, code(KeyCode::Enter)), Command::None));
        assert!(
            s.notice.as_ref().unwrap().contains("1-63"),
            "实际: {:?}",
            s.notice
        );
    }

    #[test]
    fn esc_closes_any_dialog() {
        let mut s = ready();
        map_key(&mut s, ctrl('o'));
        map_key(&mut s, code(KeyCode::Esc));
        assert!(s.dialog.is_none());
    }

    #[test]
    fn ctrl_k_opens_control_menu_and_arrows_move_selection() {
        let mut s = ready();
        map_key(&mut s, ctrl('k'));
        assert_eq!(s.dialog, Some(Dialog::Control { selected: 0 }));
        map_key(&mut s, code(KeyCode::Down));
        assert_eq!(s.dialog, Some(Dialog::Control { selected: 1 }));
        map_key(&mut s, code(KeyCode::Up));
        assert_eq!(s.dialog, Some(Dialog::Control { selected: 0 }));
    }

    #[test]
    fn control_menu_enter_emits_the_selected_command() {
        let mut s = ready();
        map_key(&mut s, ctrl('k'));
        map_key(&mut s, code(KeyCode::Down)); // 关闭当前通道
        match map_key(&mut s, code(KeyCode::Enter)) {
            Command::CloseDlc { dlci } => assert_eq!(dlci, 1),
            other => panic!("期望 CloseDlc，得到 {other:?}"),
        }

        map_key(&mut s, ctrl('k'));
        map_key(&mut s, code(KeyCode::Down));
        map_key(&mut s, code(KeyCode::Down)); // CLD
        assert!(matches!(
            map_key(&mut s, code(KeyCode::Enter)),
            Command::CloseMux
        ));
    }

    #[test]
    fn typing_while_dialog_open_does_not_touch_input_box() {
        let mut s = ready();
        map_key(&mut s, ctrl('o'));
        map_key(&mut s, key('5'));
        assert_eq!(s.input.buffer, "", "弹窗打开时不应写入输入框");
    }

    #[test]
    fn ctrl_g_opens_help_and_any_key_closes_it() {
        let mut s = ready();
        map_key(&mut s, ctrl('g'));
        assert_eq!(s.dialog, Some(Dialog::Help));
        // 帮助页不必记 Esc，随便按一个键就关
        map_key(&mut s, key('x'));
        assert!(s.dialog.is_none(), "任意键应关闭帮助");
        assert_eq!(s.input.buffer, "", "关闭帮助的按键不该落进输入框");
    }

    #[test]
    fn ctrl_s_toggles_logging() {
        let mut s = ready();
        assert!(matches!(map_key(&mut s, ctrl('s')), Command::ToggleLog));
    }

    #[test]
    fn history_keys_recall_previous_input() {
        let mut s = ready();
        for c in "AT".chars() {
            map_key(&mut s, key(c));
        }
        map_key(&mut s, code(KeyCode::Enter));
        map_key(&mut s, code(KeyCode::Up));
        assert_eq!(s.input.buffer, "AT");
    }
}
