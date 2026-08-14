//! 原始帧调试视图：物理串口层面的每一帧及其解析结果。

use crate::app::record::hexdump_lines;
use crate::app::state::AppState;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// 全屏覆盖渲染原始帧日志。
pub fn render_raw(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 原始帧 — Ctrl+R 返回 ")
        .border_style(Style::default().fg(Color::Magenta));

    let inner = block.inner(area);
    let per_line = if inner.width >= 66 { 16 } else { 8 };
    let height = inner.height as usize;

    let mut lines: Vec<Line> = Vec::new();
    for e in state.raw_log.iter().rev() {
        let mut chunk: Vec<Line> = Vec::new();
        let color = if !e.ok {
            Color::Red
        } else if e.outgoing {
            Color::Cyan
        } else {
            Color::Green
        };
        chunk.push(Line::from(Span::styled(e.line(), Style::default().fg(color))));
        for (hex, ascii) in hexdump_lines(&e.bytes, per_line) {
            chunk.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(hex, Style::default().fg(Color::Gray)),
                Span::raw("  "),
                Span::styled(ascii, Style::default().fg(Color::DarkGray)),
            ]));
        }
        chunk.reverse();
        for l in chunk {
            lines.push(l);
            if lines.len() >= height {
                break;
            }
        }
        if lines.len() >= height {
            break;
        }
    }
    lines.reverse();

    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::{AppConfig, AppState, RawEntry};
    use crate::mux::frame::Frame as MuxFrame;
    use crate::ui::testutil::render_text;

    fn render(state: &AppState, w: u16, h: u16) -> String {
        render_text(w, h, |f| render_raw(f, f.area(), state))
    }

    #[test]
    fn shows_frame_summaries() {
        let mut s = AppState::new(AppConfig::default());
        s.push_raw(RawEntry::from_frame(&MuxFrame::sabm(1), true));
        let out = render(&s, 70, 10);
        assert!(out.contains("SABM"), "实际:\n{out}");
        assert!(out.contains("TX"), "实际:\n{out}");
    }

    #[test]
    fn marks_fcs_failures() {
        let mut s = AppState::new(AppConfig::default());
        s.push_raw(RawEntry::bad("FCS 不匹配", vec![0xF9, 0x03, 0x3F]));
        let out = render(&s, 70, 10);
        assert!(out.contains("[FCS 失败]"), "实际:\n{out}");
    }

    #[test]
    fn shows_raw_bytes_in_hex() {
        let mut s = AppState::new(AppConfig::default());
        s.push_raw(RawEntry::bad("垃圾", vec![0xDE, 0xAD]));
        let out = render(&s, 70, 10);
        assert!(out.contains("DE AD"), "实际:\n{out}");
    }

    #[test]
    fn title_explains_how_to_exit() {
        let s = AppState::new(AppConfig::default());
        let out = render(&s, 70, 10);
        assert!(out.contains("Ctrl+R"), "实际:\n{out}");
    }

    #[test]
    fn empty_log_renders_without_panic() {
        let s = AppState::new(AppConfig::default());
        let _ = render(&s, 20, 4);
    }
}
