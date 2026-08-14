//! 居中弹窗：通道绑定与控制通道命令菜单。

use crate::app::state::{CONTROL_ITEMS, Dialog, HELP_KEYS};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// 在 `area` 中取一块居中的矩形。
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

/// 渲染当前弹窗。
pub fn render_dialog(frame: &mut Frame, area: Rect, dialog: &Dialog) {
    let (title, lines, w, h) = match dialog {
        Dialog::Bind { slot, input } => {
            let lines = vec![
                Line::from(format!("  绑定到槽位 {}", slot + 1)),
                Line::from(""),
                Line::from(vec![
                    Span::raw("  DLCI: "),
                    Span::styled(input.clone(), Style::default().fg(Color::Yellow)),
                    Span::styled("▌", Style::default().fg(Color::Yellow)),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    "  取值 1-63，回车确认，Esc 取消",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            (" 打开通道 ", lines, 44u16, 7u16)
        }
        Dialog::Control { selected } => {
            let mut lines = vec![Line::from("")];
            for (i, item) in CONTROL_ITEMS.iter().enumerate() {
                let marked = i == *selected;
                let style = if marked {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                lines.push(Line::from(Span::styled(
                    format!("  {} {}", if marked { '▶' } else { ' ' }, item),
                    style,
                )));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  ↑↓ 选择，回车执行，Esc 取消",
                Style::default().fg(Color::DarkGray),
            )));
            (" 控制通道命令 ", lines, 48u16, 8u16)
        }
        Dialog::Help => {
            let width = HELP_KEYS.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
            let mut lines = vec![Line::from("")];
            for (key, what) in HELP_KEYS {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {key:<width$}  "),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::raw(what),
                ]));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  按任意键关闭",
                Style::default().fg(Color::DarkGray),
            )));
            (
                " 快捷键 ",
                lines,
                58u16,
                (HELP_KEYS.len() + 5) as u16,
            )
        }
    };

    let rect = centered(area, w, h);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Yellow));

    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::Dialog;
    use crate::ui::testutil::render_text;

    fn render(d: &Dialog, w: u16, h: u16) -> String {
        render_text(w, h, |f| render_dialog(f, f.area(), d))
    }

    #[test]
    fn bind_dialog_shows_prompt_and_typed_digits() {
        let out = render(&Dialog::Bind { slot: 0, input: "12".into() }, 60, 12);
        assert!(out.contains("DLCI"), "实际:\n{out}");
        assert!(out.contains("12"), "实际:\n{out}");
        assert!(out.contains("1-63"), "应提示取值范围，实际:\n{out}");
    }

    #[test]
    fn bind_dialog_names_the_target_slot() {
        let out = render(&Dialog::Bind { slot: 1, input: String::new() }, 60, 12);
        assert!(out.contains("槽位 2"), "实际:\n{out}");
    }

    #[test]
    fn control_dialog_lists_all_items() {
        let out = render(&Dialog::Control { selected: 0 }, 60, 12);
        assert!(out.contains("MSC"), "实际:\n{out}");
        assert!(out.contains("DISC"), "实际:\n{out}");
        assert!(out.contains("CLD"), "实际:\n{out}");
    }

    #[test]
    fn control_dialog_marks_selection() {
        let out = render(&Dialog::Control { selected: 1 }, 60, 12);
        let line = out.lines().find(|l| l.contains("DISC")).unwrap();
        assert!(line.contains('▶'), "选中项应有标记，实际: {line}");
    }

    #[test]
    fn help_lists_every_shortcut() {
        use crate::app::state::HELP_KEYS;
        let out = render(&Dialog::Help, 70, 24);
        assert!(!HELP_KEYS.is_empty(), "帮助表不能是空的");
        for (key, _) in HELP_KEYS {
            assert!(out.contains(key), "帮助里缺少 {key}:\n{out}");
        }
    }

    #[test]
    fn help_says_how_to_close_itself() {
        let out = render(&Dialog::Help, 70, 24);
        assert!(out.contains("按任意键关闭"), "实际:\n{out}");
    }

    #[test]
    fn tiny_area_does_not_panic() {
        let _ = render(&Dialog::Control { selected: 0 }, 10, 4);
        let _ = render(&Dialog::Bind { slot: 0, input: String::new() }, 6, 3);
    }
}
