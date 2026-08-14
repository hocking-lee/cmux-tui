//! 顶层布局：状态栏 + 两个通道面板 + 输入框，以及覆盖层。

use crate::app::state::{AppState, LinkState};
use crate::ui::dialog::render_dialog;
use crate::ui::hexview::render_channel;
use crate::ui::raw::render_raw;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction as LayoutDir, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

fn link_span(link: LinkState) -> Span<'static> {
    let (text, color) = match link {
        LinkState::Up => ("● MUX UP", Color::Green),
        LinkState::Negotiating => ("● 握手中", Color::Yellow),
        LinkState::Down => ("● MUX DOWN", Color::Red),
    };
    Span::styled(text, Style::default().fg(color))
}

fn status_line(state: &AppState) -> Line<'static> {
    let s = &state.stats;
    Line::from(vec![
        Span::raw(format!(" {} ", state.port_desc)),
        Span::raw("│ "),
        link_span(state.link),
        Span::raw(format!(
            " │ rx {} tx {} │ fcs_err {} ",
            s.rx_frames, s.tx_frames, s.fcs_errors
        )),
    ])
}

fn input_line(state: &AppState) -> Line<'static> {
    let target = match state.target_dlci() {
        Some(d) => format!("→ DLCI {d}"),
        None => "→ 无通道".to_string(),
    };
    let mut spans = vec![
        Span::styled(
            format!(" {} ", state.input.mode.label()),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
        Span::raw(" "),
        Span::raw(state.input.buffer.clone()),
        Span::styled("▌", Style::default().fg(Color::Yellow)),
        Span::raw("  "),
        Span::styled(target, Style::default().fg(Color::DarkGray)),
    ];
    if let Some(n) = &state.notice {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(n.clone(), Style::default().fg(Color::Red)));
    }
    Line::from(spans)
}

/// 绘制整个界面。
pub fn draw(frame: &mut Frame, state: &AppState) {
    let rows = Layout::default()
        .direction(LayoutDir::Vertical)
        .constraints([
            Constraint::Length(1), // 状态栏
            Constraint::Min(3),    // 通道面板
            Constraint::Length(3), // 输入框
        ])
        .split(frame.area());

    frame.render_widget(
        Paragraph::new(status_line(state)).style(Style::default().bg(Color::Rgb(28, 28, 34))),
        rows[0],
    );

    let cols = Layout::default()
        .direction(LayoutDir::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);

    for (i, slot) in state.slots.iter().enumerate() {
        if let Some(area) = cols.get(i) {
            render_channel(frame, *area, slot, i, state.focus == i);
        }
    }

    frame.render_widget(
        Paragraph::new(input_line(state)).block(Block::default().borders(Borders::ALL)),
        rows[2],
    );

    // 覆盖层：原始帧视图在下，弹窗在最上。
    if state.show_raw {
        render_raw(frame, frame.area(), state);
    }
    if let Some(d) = &state.dialog {
        render_dialog(frame, frame.area(), d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::input::InputMode;
    use crate::app::state::{AppConfig, AppState, LinkState};
    use crate::ui::testutil::render_text;

    fn render(state: &AppState, w: u16, h: u16) -> String {
        render_text(w, h, |f| draw(f, state))
    }

    fn ready_state() -> AppState {
        let mut s = AppState::new(AppConfig::default());
        s.port_desc = "/dev/ttyUSB0 115200 8N1".into();
        s.link = LinkState::Up;
        s.bind(1);
        s.bind(3);
        s
    }

    #[test]
    fn status_bar_shows_port_and_link_state() {
        let out = render(&ready_state(), 90, 20);
        let first = out.lines().next().unwrap();
        assert!(first.contains("/dev/ttyUSB0"), "实际: {first}");
        assert!(first.contains("MUX UP"), "实际: {first}");
    }

    #[test]
    fn status_bar_shows_counters() {
        let mut s = ready_state();
        s.stats.rx_frames = 12;
        s.stats.tx_frames = 3;
        s.stats.fcs_errors = 1;
        let first = render(&s, 90, 20).lines().next().unwrap().to_string();
        assert!(first.contains("rx 12"), "实际: {first}");
        assert!(first.contains("tx 3"), "实际: {first}");
        assert!(first.contains("fcs_err 1"), "实际: {first}");
    }

    #[test]
    fn link_down_is_visible() {
        let mut s = ready_state();
        s.link = LinkState::Down;
        let first = render(&s, 90, 20).lines().next().unwrap().to_string();
        assert!(first.contains("MUX DOWN"), "实际: {first}");
    }

    #[test]
    fn both_channel_panels_are_drawn_side_by_side() {
        let out = render(&ready_state(), 90, 20);
        let panel_row = out.lines().nth(1).unwrap();
        assert!(panel_row.contains("DLCI 1"), "实际: {panel_row}");
        assert!(panel_row.contains("DLCI 3"), "实际: {panel_row}");
    }

    #[test]
    fn input_bar_shows_mode_and_target() {
        let out = render(&ready_state(), 90, 20);
        assert!(out.contains("ASCII"), "应显示输入模式");
        assert!(out.contains("→ DLCI 1"), "应显示发送目标，实际:\n{out}");
    }

    #[test]
    fn input_bar_reflects_hex_mode_and_typed_text() {
        let mut s = ready_state();
        s.input.toggle_mode();
        for c in "41 54".chars() {
            s.input.insert(c);
        }
        assert_eq!(s.input.mode, InputMode::Hex);
        let joined = render(&s, 90, 20);
        assert!(joined.contains("HEX"), "实际:\n{joined}");
        assert!(joined.contains("41 54"), "实际:\n{joined}");
    }

    #[test]
    fn notice_is_displayed_when_present() {
        let mut s = ready_state();
        s.notice = Some("十六进制位数必须为偶数".into());
        let joined = render(&s, 90, 20);
        assert!(joined.contains("十六进制位数必须为偶数"), "实际:\n{joined}");
    }

    #[test]
    fn empty_target_shows_placeholder() {
        let mut s = AppState::new(AppConfig::default());
        s.port_desc = "/dev/ttyUSB0".into();
        let joined = render(&s, 90, 20);
        assert!(joined.contains("→ 无通道"), "实际:\n{joined}");
    }

    #[test]
    fn renders_without_panic_on_tiny_terminal() {
        // 极小终端不能 panic
        let _ = render(&ready_state(), 20, 6);
        let _ = render(&ready_state(), 8, 3);
    }

    #[test]
    fn raw_view_overlays_the_main_screen() {
        let mut s = ready_state();
        s.show_raw = true;
        let out = render(&s, 90, 20);
        assert!(out.contains("原始帧"), "实际:\n{out}");
    }

    #[test]
    fn dialog_overlays_everything() {
        use crate::app::state::Dialog;
        let mut s = ready_state();
        s.show_raw = true;
        s.dialog = Some(Dialog::Control { selected: 0 });
        let out = render(&s, 90, 20);
        assert!(out.contains("控制通道命令"), "弹窗应在最上层，实际:\n{out}");
    }
}
