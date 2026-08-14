//! UI 测试辅助：把 `TestBackend` 的单元格缓冲还原成可断言的文本行。
//!
//! 直接逐单元格拼接是不行的：东亚宽字符在终端占两列，ratatui 会把字符放在
//! 第一个单元格、让第二个单元格留空，逐格拼接会得到「建 链 成 功」这种
//! 夹了空格的结果，导致断言在渲染完全正确时失败。这里按字符宽度跳过占位格。

use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// 判断字符是否按东亚宽字符渲染（占两列）。
///
/// 取 Unicode East Asian Wide / Fullwidth 的主要区段，够覆盖界面中出现的
/// 中文与全角标点。`◀`(U+25C0)、`●`(U+25CF) 属于 Ambiguous，按一列处理，
/// 与 ratatui 的判定一致。
fn is_wide(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x20000..=0x3FFFD
    )
}

/// 在给定尺寸的测试后端上绘制一次，返回逐行文本（右侧空白已裁掉）。
pub fn render_lines<F>(w: u16, h: u16, draw: F) -> Vec<String>
where
    F: FnOnce(&mut Frame),
{
    let backend = TestBackend::new(w, h);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(draw).unwrap();
    let buf = term.backend().buffer().clone();

    (0..h)
        .map(|y| {
            let mut line = String::new();
            let mut x = 0u16;
            while x < w {
                let sym = buf[(x, y)].symbol();
                line.push_str(sym);
                // 宽字符后面跟着一个占位单元格，跳过它。
                let step = match sym.chars().next() {
                    Some(c) if is_wide(c) => 2,
                    _ => 1,
                };
                x += step;
            }
            line.trim_end().to_string()
        })
        .collect()
}

/// 同 `render_lines`，但直接拼成一个多行字符串，便于整体 contains 断言。
pub fn render_text<F>(w: u16, h: u16, draw: F) -> String
where
    F: FnOnce(&mut Frame),
{
    render_lines(w, h, draw).join("\n")
}
