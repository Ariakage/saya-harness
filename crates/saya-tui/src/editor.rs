//! Bounded UTF-8 input editing with grapheme-aware cursor movement.
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const MAX_INPUT_BYTES: usize = 65_536;
pub(crate) const MAX_GRAPHEME_BYTES: usize = 128;
pub(crate) const MAX_VISUAL_LINE_BYTES: usize = 4096;

#[derive(Debug, Default)]
pub struct Editor {
    text: String,
    cursor: usize,
    pub truncated: bool,
}

impl Editor {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replace the draft with bounded UTF-8 text and place the cursor at its end.
    /// The return value indicates whether the complete input was retained.
    pub fn set_text(&mut self, text: impl AsRef<str>) -> bool {
        self.text.clear();
        self.cursor = 0;
        self.truncated = false;
        self.insert(text.as_ref())
    }

    /// Insert at the current grapheme boundary. Oversized pastes retain a prefix
    /// and set an explicit display flag without exceeding the input budget.
    pub fn insert(&mut self, text: &str) -> bool {
        let remaining = MAX_INPUT_BYTES.saturating_sub(self.text.len());
        let end = prefix_boundary(text, remaining);
        self.text.insert_str(self.cursor, &text[..end]);
        self.cursor += end;
        self.normalize_cursor();
        let complete = end == text.len();
        self.truncated |= !complete;
        complete
    }

    /// Normalize terminal paste line endings before bounded insertion.
    pub fn insert_paste(&mut self, text: &str) -> bool {
        if text.contains('\r') {
            self.insert(&text.replace("\r\n", "\n").replace('\r', "\n"))
        } else {
            self.insert(text)
        }
    }

    pub fn take(&mut self) -> String {
        self.cursor = 0;
        self.truncated = false;
        std::mem::take(&mut self.text)
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.truncated = false;
    }

    pub fn move_left(&mut self) {
        self.cursor = self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(index, _)| index);
    }

    pub fn move_right(&mut self) {
        if let Some(grapheme) = self.text[self.cursor..].graphemes(true).next() {
            self.cursor += grapheme.len();
        }
    }

    pub fn home(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
    }

    pub fn end(&mut self) {
        self.cursor += self.text[self.cursor..]
            .find('\n')
            .unwrap_or(self.text.len() - self.cursor);
    }

    pub fn move_up(&mut self) {
        let line_start = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        if line_start == 0 {
            return;
        }
        let column = display_width(&self.text[line_start..self.cursor]);
        let previous_end = line_start - 1;
        let previous_start = self.text[..previous_end]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        self.cursor =
            previous_start + boundary_at_column(&self.text[previous_start..previous_end], column);
    }

    pub fn move_down(&mut self) {
        let line_start = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let column = display_width(&self.text[line_start..self.cursor]);
        let Some(next_break) = self.text[self.cursor..].find('\n') else {
            return;
        };
        let next_start = self.cursor + next_break + 1;
        let next_end = next_start
            + self.text[next_start..]
                .find('\n')
                .unwrap_or(self.text.len() - next_start);
        self.cursor = next_start + boundary_at_column(&self.text[next_start..next_end], column);
    }

    pub fn backspace(&mut self) {
        let end = self.cursor;
        self.move_left();
        self.text.replace_range(self.cursor..end, "");
        self.normalize_cursor();
    }

    pub fn delete(&mut self) {
        let start = self.cursor;
        self.move_right();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.normalize_cursor();
    }

    /// Return the logical line and visible display column for the input cursor.
    pub fn cursor_position(&self) -> (usize, usize) {
        let prefix = &self.text[..self.cursor];
        let row = prefix.bytes().filter(|byte| *byte == b'\n').count();
        let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
        (row, display_width(&prefix[line_start..]))
    }

    /// Handle editing keys. Submission and global commands belong to the controller.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }
        match key.code {
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.insert("\n");
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert("\n");
            }
            KeyCode::Char(character)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                self.insert(&character.to_string());
            }
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Up => self.move_up(),
            KeyCode::Down => self.move_down(),
            KeyCode::Home => self.home(),
            KeyCode::End => self.end(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Tab => {
                self.insert("\t");
            }
            _ => return false,
        }
        true
    }

    fn normalize_cursor(&mut self) {
        self.cursor = self
            .text
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .chain(std::iter::once(self.text.len()))
            .find(|index| *index >= self.cursor)
            .unwrap_or(self.text.len());
    }
}

pub(crate) fn prefix_boundary(text: &str, max_bytes: usize) -> usize {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Make terminal controls and bidirectional overrides visible as text.
/// Tabs use four display cells while line feeds retain their layout role.
pub fn safe_text(text: &str) -> String {
    let mut visible = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\n' => visible.push('\n'),
            '\t' => visible.push_str("    "),
            character
                if character.is_control()
                    || matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') =>
            {
                use std::fmt::Write;
                let _ = write!(visible, "\\u{{{:x}}}", character as u32);
            }
            character => visible.push(character),
        }
    }
    visible
}

fn display_width(text: &str) -> usize {
    safe_text(text)
        .graphemes(true)
        .map(display_grapheme)
        .map(UnicodeWidthStr::width)
        .sum()
}

pub(crate) fn display_grapheme(grapheme: &str) -> &str {
    if grapheme.len() > MAX_GRAPHEME_BYTES {
        "…"
    } else {
        grapheme
    }
}

fn boundary_at_column(text: &str, desired: usize) -> usize {
    let mut column = 0;
    let mut boundary = 0;
    for (index, grapheme) in text.grapheme_indices(true) {
        let width = display_width(grapheme);
        if column + width > desired {
            break;
        }
        column += width;
        boundary = index + grapheme.len();
    }
    boundary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_and_deletion_preserve_combining_and_emoji_graphemes() {
        let mut editor = Editor::default();
        editor.insert("e\u{301}👩‍💻界");
        editor.move_left();
        editor.backspace();
        assert_eq!(editor.text(), "e\u{301}界");
        editor.move_left();
        assert_eq!(editor.cursor(), 0);
        editor.delete();
        assert_eq!(editor.text(), "界");
        assert_eq!(editor.cursor(), 0);
    }

    #[test]
    fn multiline_motion_uses_visible_columns_and_handles_empty_lines() {
        let mut editor = Editor::default();
        editor.insert("ab\n界c\n\nlast");
        editor.home();
        editor.move_up();
        assert_eq!(editor.cursor_position(), (2, 0));
        editor.move_up();
        editor.move_right();
        assert_eq!(editor.cursor_position(), (1, 2));
        editor.move_up();
        assert_eq!(editor.cursor_position(), (0, 2));
        editor.move_down();
        assert_eq!(editor.cursor_position(), (1, 2));
        editor.end();
        assert_eq!(editor.cursor_position(), (1, 3));
    }

    #[test]
    fn paste_limits_are_utf8_safe_and_controls_are_visible() {
        let mut editor = Editor::default();
        assert!(!editor.insert(&"界".repeat(MAX_INPUT_BYTES)));
        assert!(editor.text().len() <= MAX_INPUT_BYTES);
        assert!(editor.truncated);
        editor.take();
        assert!(!editor.truncated);
        editor.insert("\u{301}");
        editor.home();
        editor.insert("e");
        assert_eq!(editor.cursor(), editor.text().len());
        assert_eq!(
            safe_text("a\x1b[2J\u{9b}x\u{202e}\t\n"),
            "a\\u{1b}[2J\\u{9b}x\\u{202e}    \n"
        );
    }

    #[test]
    fn terminal_paste_normalizes_crlf_and_cr_before_editing() {
        let mut editor = Editor::default();
        assert!(editor.insert_paste("first\r\n二🌸\rthird\n"));
        assert_eq!(editor.text(), "first\n二🌸\nthird\n");
        editor.move_up();
        editor.home();
        editor.end();
        assert_eq!(editor.cursor_position(), (2, 5));
        assert!(!editor.text().contains('\r'));
        editor.set_text("prefix");
        assert!(!editor.insert_paste(&"\r\n".repeat(MAX_INPUT_BYTES)));
        assert!(editor.text().len() <= MAX_INPUT_BYTES);
        assert!(!editor.text().contains('\r'));
        assert!(editor.truncated);
    }

    #[test]
    fn cursor_columns_follow_replacement_of_oversized_graphemes() {
        let mut editor = Editor::default();
        let cluster = format!("👩{}", "\u{200d}👩".repeat(100));
        editor.set_text(format!("{cluster}x"));
        assert_eq!(editor.text(), format!("{cluster}x"));
        assert_eq!(editor.cursor_position(), (0, 2));
        editor.move_left();
        assert_eq!(editor.cursor_position(), (0, 1));
        editor.home();
        assert_eq!(editor.cursor_position(), (0, 0));
    }
}
