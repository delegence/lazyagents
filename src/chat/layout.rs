use std::io::{self, Write};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::commands::Command;
use super::editor::Editor;

// Keep wrapping enabled so the terminal can reflow normal scrollback on resize.
// Save the top of the editable region after each draw; the terminal moves this
// saved cursor with its text during reflow. OSC 133 marks this transient region
// so supporting terminals also remove parts pushed into scrollback by a shrink.
pub(super) struct Display {
    rows: usize,
    cursor_row: usize,
}

struct Layout {
    rows: Vec<String>,
    cursor: (usize, usize),
}

fn wrap(text: &str, width: usize, cursor: Option<usize>) -> Layout {
    let width = width.max(1);
    let mut rows = vec![String::new()];
    let mut column = 0;
    let mut position = (0, 0);
    let mut offset = 0;
    while offset < text.len() {
        if cursor == Some(offset) {
            position = if column == width {
                (rows.len(), 0)
            } else {
                (rows.len() - 1, column)
            };
        }
        // Only internally generated SGR styles reach this function.
        if text[offset..].starts_with("\x1b[") {
            if let Some(end) = text[offset..].find('m') {
                rows.last_mut()
                    .unwrap()
                    .push_str(&text[offset..offset + end + 1]);
                offset += end + 1;
                continue;
            }
        }
        let g = text[offset..].graphemes(true).next().unwrap();
        if g == "\n" {
            rows.push(String::new());
            column = 0;
        } else {
            let cells = if g == "\t" {
                8 - column % 8
            } else {
                UnicodeWidthStr::width(g).max(1)
            };
            if g != "\t" && (column == width || column + cells.min(width) > width) {
                rows.push(String::new());
                column = 0;
                if cursor == Some(offset) {
                    position = (rows.len() - 1, 0);
                }
            }
            if g == "\t" {
                for _ in 0..cells {
                    if column == width {
                        rows.push(String::new());
                        column = 0;
                    }
                    rows.last_mut().unwrap().push(' ');
                    column += 1;
                }
            } else if cells > width {
                // A two-cell grapheme cannot fit in a one-column terminal.
                rows.last_mut().unwrap().push('�');
                column += 1;
            } else {
                if UnicodeWidthStr::width(g) == 0 {
                    rows.last_mut().unwrap().push('◌');
                }
                rows.last_mut().unwrap().push_str(g);
                column += cells;
            }
        }
        offset += g.len();
    }
    if cursor == Some(text.len()) {
        if column == width {
            rows.push(String::new());
            column = 0;
        }
        position = (rows.len() - 1, column);
    } else if position.0 == rows.len() {
        rows.push(String::new());
    }
    Layout {
        rows,
        cursor: position,
    }
}

fn layout(editor: &Editor, size: (u16, u16)) -> Layout {
    let width = usize::from(size.0.max(1));
    let height = usize::from(size.1.max(1));
    let mut rows = Vec::new();
    // Keep prompt and input graphemes separate. A leading combining mark must
    // not join the prompt's last space and hide the cursor's byte boundary.
    let prefix = format!("{}\x1b[0m", editor.prompt());
    let input = wrap(
        &format!("{prefix}{}", editor.text),
        width,
        Some(editor.cursor + prefix.len()),
    );
    let choices = editor.choices();
    let menu_count = choices.len().min(height.saturating_sub(1));
    let capacity = (height - menu_count).min(8);
    let start = input
        .cursor
        .0
        .saturating_add(1)
        .saturating_sub(capacity)
        .min(input.rows.len().saturating_sub(capacity));
    let cursor = (rows.len() + input.cursor.0 - start, input.cursor.1);
    rows.extend(input.rows.into_iter().skip(start).take(capacity));
    let menu_start = editor.selected.saturating_add(1).saturating_sub(menu_count);
    for (index, command) in choices.iter().enumerate().skip(menu_start).take(menu_count) {
        let pointer = if index == editor.selected { ">" } else { " " };
        let text = format!("{pointer} {:<13} {}", command.name, command.description);
        let text = wrap(&text, width, None).rows.remove(0);
        let color = if index == editor.selected {
            "97"
        } else {
            "38;5;250"
        };
        rows.push(if std::env::var_os("NO_COLOR").is_some() {
            text
        } else {
            format!("\x1b[{color}m{text}\x1b[0m")
        });
    }
    Layout { rows, cursor }
}

fn up(out: &mut impl Write, rows: usize) -> io::Result<()> {
    if rows > 0 {
        write!(out, "\x1b[{rows}A")?;
    }
    Ok(())
}

impl Display {
    pub fn new() -> Self {
        Self {
            rows: 0,
            cursor_row: 0,
        }
    }

    pub fn resized(&mut self, out: &mut impl Write) -> io::Result<()> {
        // Reflow changes the number of rows above and inside our region. Restore
        // its saved start instead of moving up by the previous physical height.
        // Everything below this point is editor output or unused terminal rows.
        write!(out, "\x1b[?2026h\x1b8\r\x1b[J")?;
        self.rows = 0;
        self.cursor_row = 0;
        Ok(())
    }

    fn clear(&mut self, out: &mut impl Write, height: u16) -> io::Result<()> {
        // On a height reduction, the terminal may have scrolled the region's top.
        let height = usize::from(height.max(1));
        let cursor = self.cursor_row.min(height - 1);
        up(out, cursor)?;
        write!(out, "\r")?;
        let rows = self.rows.min(height);
        for row in 0..rows {
            write!(out, "\x1b[K")?;
            if row + 1 < rows {
                write!(out, "\x1b[1B\r")?;
            }
        }
        up(out, rows.saturating_sub(1))?;
        self.rows = 0;
        self.cursor_row = 0;
        Ok(())
    }

    pub fn draw(
        &mut self,
        out: &mut impl Write,
        editor: &Editor,
        size: (u16, u16),
    ) -> io::Result<()> {
        let layout = layout(editor, size);
        write!(out, "\x1b[?2026h\x1b[?25l\x1b[?7h")?;
        self.clear(out, size.1)?;
        write!(out, "\x1b]133;A;redraw=1\x07")?;
        for (index, row) in layout.rows.iter().enumerate() {
            if index > 0 {
                write!(out, "\r\n")?;
            }
            if index > 0 {
                write!(out, "\x1b]133;P;k=s\x07")?;
            }
            write!(out, "{row}")?;
        }
        up(out, layout.rows.len() - 1)?;
        // Save after drawing: output may have scrolled to make room for input.
        write!(out, "\r\x1b[0m\x1b7")?;
        if layout.cursor.0 > 0 {
            write!(out, "\x1b[{}B", layout.cursor.0)?;
        }
        write!(out, "\x1b[?25h\x1b[?2026l\r")?;
        if layout.cursor.1 > 0 {
            write!(out, "\x1b[{}C", layout.cursor.1)?;
        }
        out.flush()?;
        self.rows = layout.rows.len();
        self.cursor_row = layout.cursor.0;
        Ok(())
    }

    pub fn finish(&mut self, out: &mut impl Write, line: &str, size: (u16, u16)) -> io::Result<()> {
        write!(out, "\x1b[?2026h\x1b[?25l")?;
        self.clear(out, size.1)?;
        write!(out, "\x1b]133;C\x07")?;
        write!(out, "\x1b[?7h")?;
        if !line.trim().is_empty() && Command::parse(line.trim()).is_none() {
            write!(
                out,
                "\x1b[0;97m| \x1b[0m\x1b[0;1;97m{}\x1b[0m\r\n",
                line.replace('\n', "\r\n")
            )?;
        }
        write!(out, "\x1b[0m\x1b[?25h\x1b[?2026l")?;
        out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::input::ChatHeader;

    #[test]
    fn unicode_wrapping_and_cursor_use_terminal_cells() {
        let input = wrap("> e\u{301}👩🏽‍💻界", 5, Some("> e\u{301}👩🏽‍💻界".len()));
        assert_eq!(input.rows, ["> e\u{301}👩🏽‍💻", "界"]);
        assert_eq!(input.cursor, (1, 2));
        assert_eq!(wrap("> abc", 5, Some(5)).cursor, (1, 0));
        assert_eq!(wrap("界", 1, Some(3)).rows, ["�", ""]);
        assert_eq!(wrap("x\ty", 4, Some(3)).cursor, (2, 1));
    }

    #[test]
    fn standalone_combining_marks_have_visible_cells_and_safe_cursors() {
        let mut editor = Editor::new(vec![], "\u{301}abc\n\u{200d}界".into());
        for (offset, expected) in [(0, (0, 2)), (2, (0, 3)), (6, (1, 0)), (9, (1, 1))] {
            editor.cursor = offset;
            let frame = layout(&editor, (40, 24));
            assert_eq!(frame.cursor, expected);
            assert!(frame.rows[0].contains("◌\u{301}abc"));
            assert!(frame.rows[1].contains("◌\u{200d}界"));
        }
        assert_eq!(editor.text, "\u{301}abc\n\u{200d}界");
    }

    // Run by tests/reflow.mjs with the Ghostty VT engine. vt100 does not reflow.
    #[test]
    #[ignore]
    fn export_reflow_trace() {
        use serde_json::json;
        let header = ChatHeader::new(
            "coach",
            crate::config::AgentColor::Cyan,
            "pi",
            "model",
            "medium",
            crate::permissions::Mode::FullAccess,
        );
        let mut ops = vec![
            json!({"write": format!("PREVIOUS shell output with an intact long suffix SENTINEL\r\nAssistant response that must return after resizing RESPONSE_END\r\n{}", header.render(true).replace('\n',"\r\n"))}),
        ];
        let mut display = Display::new();
        let mut editor = Editor::new(vec![], "draft with a long suffix DRAFT_END".into());
        let sizes = [
            (80, 30),
            (12, 30),
            (100, 30),
            (24, 18),
            (80, 30),
            (5, 10),
            (100, 30),
            (1, 1),
            (100, 30),
        ];
        for (i, size) in sizes.into_iter().enumerate() {
            let mut bytes = vec![];
            if i > 0 {
                ops.push(json!({"resize": [size.0,size.1]}));
                display.resized(&mut bytes).unwrap();
            }
            display.draw(&mut bytes, &editor, size).unwrap();
            ops.push(json!({"write": String::from_utf8(bytes).unwrap()}));
            if size.0 >= 80 {
                ops.push(json!({"check": "draft"}));
            }
        }
        // Multiple signals can arrive before the application gets a resize event.
        for size in [(3, 3), (6, 4), (20, 12), (100, 30)] {
            ops.push(json!({"resize": [size.0,size.1]}));
        }
        let mut bytes = vec![];
        display.resized(&mut bytes).unwrap();
        display.draw(&mut bytes, &editor, (100, 30)).unwrap();
        ops.push(json!({"write": String::from_utf8(bytes).unwrap()}));
        ops.push(json!({"check": "draft"}));
        editor.text = "/status".into();
        editor.cursor = editor.text.len();
        let mut bytes = vec![];
        display.draw(&mut bytes, &editor, (100, 30)).unwrap();
        display.finish(&mut bytes, "/status", (100, 30)).unwrap();
        ops.push(json!({"write": String::from_utf8(bytes).unwrap()}));
        ops.push(json!({"write": "status result STATUS_END\r\n"}));
        ops.push(json!({"check": "finished"}));
        // Once submitted, later terminal resizes must not treat output as a prompt.
        ops.push(json!({"resize": [12, 12]}));
        ops.push(json!({"resize": [100, 30]}));
        ops.push(json!({"check": "finished"}));
        std::fs::write(
            std::env::var("LAZYAGENTS_REFLOW_TRACE").unwrap(),
            serde_json::to_vec(&ops).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn viewport_keeps_cursor_visible_without_borders() {
        let mut editor = Editor::new(vec![], "界e\u{301}👩🏽‍💻\n".repeat(100));
        for width in [1, 2, 3, 8, 24, 100] {
            for height in [1, 2, 3, 8, 24] {
                for cursor in [0, "界".len(), editor.text.len()] {
                    editor.cursor = cursor;
                    let frame = layout(&editor, (width, height));
                    assert!(frame.rows.len() <= usize::from(height));
                    assert!(frame.cursor.0 < frame.rows.len());
                    assert!(frame.cursor.1 < usize::from(width));
                    assert!(frame.rows.iter().all(|row| !row.contains('─')));
                }
            }
        }
    }

    #[test]
    fn redraw_resize_and_submission_preserve_surrounding_output() {
        let mut terminal = vt100::Parser::new(24, 40, 100);
        terminal.process(b"OLD\r\n");
        let mut display = Display::new();
        let mut editor = Editor::new(
            vec![],
            "a long input that wraps over several terminal rows".into(),
        );
        for (width, height) in [
            (40, 24),
            (24, 24),
            (100, 24),
            (8, 24),
            (40, 24),
            (40, 8),
            (40, 24),
        ] {
            terminal.screen_mut().set_size(height, width);
            let mut bytes = vec![];
            if display.rows > 0 {
                display.resized(&mut bytes).unwrap();
            }
            display.draw(&mut bytes, &editor, (width, height)).unwrap();
            terminal.process(&bytes);
            assert!(
                terminal.screen().contents().starts_with("OLD"),
                "{}",
                terminal.screen().contents()
            );
            let screen = terminal.screen().contents();
            assert_eq!(
                screen.matches(&"─".repeat(usize::from(width))).count(),
                0,
                "{screen}"
            );
            assert!(!terminal.screen().alternate_screen());
        }
        editor.text = "short".into();
        editor.cursor = 5;
        let mut bytes = vec![];
        display.draw(&mut bytes, &editor, (40, 24)).unwrap();
        display.finish(&mut bytes, "short", (40, 24)).unwrap();
        terminal.process(&bytes);
        assert_eq!(terminal.screen().contents(), "OLD\n| short");
        assert!(terminal.screen().cell(1, 2).unwrap().bold());
        assert_eq!(terminal.screen().cursor_position(), (2, 0));
        assert!(!bytes.windows(4).any(|b| b == b"\x1b[2J" || b == b"\x1b[3J"));
    }

    #[test]
    fn idle_editor_keeps_wrapping_enabled_and_saves_its_start() {
        let mut terminal = vt100::Parser::new(24, 40, 100);
        terminal.process(b"Earlier output must stay intact\r\n");
        let mut display = Display::new();
        let editor = Editor::new(vec![], "draft".into());
        let mut bytes = vec![];
        display.draw(&mut bytes, &editor, (40, 24)).unwrap();
        // Ghostty disables reflow for the entire screen when DECAWM is off.
        // Do not disable it even temporarily during a draw: a resize can arrive
        // before the next escape sequence restores it.
        assert!(!bytes.windows(5).any(|part| part == b"\x1b[?7l"));
        terminal.process(&bytes);
        terminal.process(b"\x1b8");
        assert_eq!(terminal.screen().cursor_position(), (1, 0));
        // Exercise wrapping, rather than only checking for a mode-set sequence.
        terminal.process(format!("{}Z", "x".repeat(40)).as_bytes());
        assert_eq!(terminal.screen().cell(2, 0).unwrap().contents(), "Z");
        assert!(terminal
            .screen()
            .contents()
            .starts_with("Earlier output must stay intact"));
    }

    #[test]
    fn submitted_commands_leave_no_text_or_blank_rows() {
        let mut terminal = vt100::Parser::new(24, 40, 100);
        terminal.process(b"Earlier output\r\n");
        let mut display = Display::new();
        let editor = Editor::new(vec![], "/s".into());
        let mut bytes = vec![];
        display.draw(&mut bytes, &editor, (40, 24)).unwrap();
        display.finish(&mut bytes, "/status", (40, 24)).unwrap();
        bytes.extend_from_slice(b"status result\r\n");
        terminal.process(&bytes);
        assert_eq!(
            terminal.screen().contents(),
            "Earlier output\nstatus result"
        );
    }
}
