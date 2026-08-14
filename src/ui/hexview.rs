//! 单个通道面板的渲染：时间戳头行 + hex 列 + ASCII 列。

use crate::app::record::{Direction, Record, hexdump_lines};
use crate::app::state::ChannelSlot;
use crate::mux::session::DlcState;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// 面板内部宽度达到该值时，每行显示 16 字节而非 8。
const WIDE_THRESHOLD: u16 = 16 * 3 + 16 + 2;

fn state_label(s: &DlcState) -> (&'static str, Color) {
    match s {
        DlcState::Open => ("UP", Color::Green),
        DlcState::Opening { .. } => ("建链中", Color::Yellow),
        DlcState::Closing { .. } => ("关闭中", Color::Yellow),
        DlcState::Failed => ("FAILED", Color::Red),
        DlcState::Closed => ("DOWN", Color::DarkGray),
    }
}

fn dir_style(dir: Direction) -> Style {
    let color = match dir {
        Direction::Tx => Color::Cyan,
        Direction::Rx => Color::Green,
        Direction::Info => Color::DarkGray,
        Direction::Error => Color::Red,
    };
    Style::default().fg(color)
}

/// 把一条记录展开成若干渲染行。
fn record_lines(rec: &Record, per_line: usize) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(Span::styled(rec.header(), dir_style(rec.dir)))];
    for (hex, ascii) in hexdump_lines(&rec.bytes, per_line) {
        out.push(Line::from(vec![
            Span::styled(hex, Style::default().fg(Color::Gray)),
            Span::raw("  "),
            Span::styled(ascii, Style::default().fg(Color::White)),
        ]));
    }
    out
}

/// 渲染一个通道面板。`index` 用于标题里的功能键提示。
pub fn render_channel(
    frame: &mut Frame,
    area: Rect,
    slot: &ChannelSlot,
    index: usize,
    focused: bool,
) {
    let title = match slot.dlci {
        Some(dlci) => {
            let (label, color) = state_label(&slot.state);
            let mut spans = vec![
                Span::raw(format!("[F{}] DLCI {} ", index + 1, dlci)),
                Span::styled(label, Style::default().fg(color)),
            ];
            if focused {
                spans.push(Span::styled(" ◀", Style::default().fg(Color::Yellow)));
            }
            Line::from(spans)
        }
        None => Line::from(format!("[F{}] 空槽位", index + 1)),
    };

    let border_style = if focused {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);

    let inner = block.inner(area);
    let per_line = if inner.width >= WIDE_THRESHOLD { 16 } else { 8 };

    let lines: Vec<Line> = if slot.dlci.is_none() {
        vec![
            Line::from(""),
            Line::from(Span::styled(
                "  按 Ctrl+O 绑定一个 DLCI",
                Style::default().fg(Color::DarkGray),
            )),
        ]
    } else {
        // 从最新往回取，直到填满可视高度，再反转成正序。
        let height = inner.height as usize;
        let skip = slot.scroll.min(slot.records.len());
        let mut collected: Vec<Line> = Vec::new();
        for rec in slot.records.iter().rev().skip(skip) {
            let mut block = record_lines(rec, per_line);
            block.reverse();
            for l in block {
                collected.push(l);
                if collected.len() >= height {
                    break;
                }
            }
            if collected.len() >= height {
                break;
            }
        }
        collected.reverse();
        collected
    };

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::record::{Direction, Record};
    use crate::app::state::ChannelSlot;
    use crate::mux::session::DlcState;
    use crate::ui::testutil::render_lines;
    use ratatui::layout::Rect;

    /// 把整个 buffer 渲染成字符串行，便于断言。
    fn render(slot: &ChannelSlot, focused: bool, w: u16, h: u16) -> Vec<String> {
        render_lines(w, h, |f| {
            render_channel(f, Rect::new(0, 0, w, h), slot, 0, focused);
        })
    }

    fn slot_with(records: Vec<Record>) -> ChannelSlot {
        ChannelSlot {
            dlci: Some(1),
            state: DlcState::Open,
            records: records.into(),
            ..Default::default()
        }
    }

    #[test]
    fn empty_slot_prompts_the_user_to_bind_one() {
        let slot = ChannelSlot::default();
        let lines = render(&slot, false, 40, 6);
        let joined = lines.join("\n");
        assert!(
            joined.contains("Ctrl+O"),
            "空槽位应提示如何绑定，实际:\n{joined}"
        );
    }

    #[test]
    fn title_shows_dlci_and_state() {
        let slot = slot_with(vec![]);
        let lines = render(&slot, false, 40, 6);
        assert!(lines[0].contains("DLCI 1"), "实际: {}", lines[0]);
        assert!(lines[0].contains("UP"), "实际: {}", lines[0]);
    }

    #[test]
    fn focused_panel_is_marked() {
        let slot = slot_with(vec![]);
        let focused = render(&slot, true, 40, 6);
        let unfocused = render(&slot, false, 40, 6);
        assert!(
            focused[0].contains('◀'),
            "焦点面板应有标记，实际: {}",
            focused[0]
        );
        assert!(!unfocused[0].contains('◀'));
    }

    #[test]
    fn record_renders_header_hex_and_ascii() {
        let slot = slot_with(vec![Record::new(Direction::Tx, 1, b"AT\r".to_vec())]);
        let lines = render(&slot, false, 40, 8);
        let joined = lines.join("\n");
        assert!(joined.contains("TX 3B"), "缺少头行，实际:\n{joined}");
        assert!(joined.contains("41 54 0D"), "缺少 hex 列，实际:\n{joined}");
        assert!(joined.contains("AT."), "缺少 ascii 列，实际:\n{joined}");
    }

    #[test]
    fn info_record_shows_note_instead_of_bytes() {
        let slot = slot_with(vec![Record::info(1, "建链成功")]);
        let joined = render(&slot, false, 40, 6).join("\n");
        assert!(joined.contains("建链成功"), "实际:\n{joined}");
    }

    #[test]
    fn narrow_panel_uses_eight_bytes_per_line() {
        let data: Vec<u8> = (0..16).collect();
        let slot = slot_with(vec![Record::new(Direction::Rx, 1, data)]);
        let joined = render(&slot, false, 40, 8).join("\n");
        // 8 字节/行时第 9 个字节 08 会另起一行，故第一行不含 "08"
        let first_hex_line = joined
            .lines()
            .find(|l| l.contains("00 01 02"))
            .expect("应有 hex 行");
        assert!(
            !first_hex_line.contains("08"),
            "窄面板应每行 8 字节，实际: {first_hex_line}"
        );
    }

    #[test]
    fn wide_panel_uses_sixteen_bytes_per_line() {
        let data: Vec<u8> = (0..16).collect();
        let slot = slot_with(vec![Record::new(Direction::Rx, 1, data)]);
        let joined = render(&slot, false, 80, 8).join("\n");
        let line = joined
            .lines()
            .find(|l| l.contains("00 01 02"))
            .expect("应有 hex 行");
        assert!(line.contains("0F"), "宽面板应每行 16 字节，实际: {line}");
    }

    #[test]
    fn newest_records_are_visible_when_following() {
        let recs: Vec<Record> = (0..50).map(|i| Record::info(1, format!("行{i}"))).collect();
        let slot = slot_with(recs);
        let joined = render(&slot, false, 40, 8).join("\n");
        assert!(
            joined.contains("行49"),
            "跟随模式应显示最新记录，实际:\n{joined}"
        );
        assert!(!joined.contains("行0\n"), "不应显示最早记录");
    }

    #[test]
    fn scrolled_view_shows_older_records() {
        let recs: Vec<Record> = (0..50).map(|i| Record::info(1, format!("行{i}"))).collect();
        let mut slot = slot_with(recs);
        slot.follow = false;
        slot.scroll = 40;
        let joined = render(&slot, false, 40, 8).join("\n");
        assert!(!joined.contains("行49"), "已滚动时不应停在最新，实际:\n{joined}");
    }
}
