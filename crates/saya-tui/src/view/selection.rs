//! Mouse selection over bounded, already-sanitized visible terminal cells.
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::Color,
};
use unicode_width::UnicodeWidthStr;

pub(crate) const MAX_COPY_BYTES: usize = 64 * 1024;
const MAX_SELECTION_CELLS: usize = 32 * 1024;
const SELECTED_BACKGROUND: Color = Color::Rgb(45, 65, 88);

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    area: Rect,
    cells: Vec<String>,
}

impl Snapshot {
    fn capture(buffer: &Buffer, area: Rect) -> Option<Self> {
        let count = usize::from(area.width) * usize::from(area.height);
        if count == 0 || count > MAX_SELECTION_CELLS {
            return None;
        }
        let mut cells = Vec::with_capacity(count);
        let mut bytes = 0;
        for y in area.y..area.bottom() {
            let mut remaining = 0;
            for x in area.x..area.right() {
                let text = if remaining > 0 {
                    remaining -= 1;
                    String::new()
                } else {
                    let text = buffer[(x, y)].symbol();
                    remaining = UnicodeWidthStr::width(text).saturating_sub(1);
                    text.to_owned()
                };
                bytes += text.len();
                if bytes > MAX_COPY_BYTES {
                    return None;
                }
                cells.push(text);
            }
        }
        Some(Self { area, cells })
    }

    fn clamp(&self, x: u16, y: u16) -> Position {
        Position::new(
            x.clamp(self.area.x, self.area.right() - 1),
            y.clamp(self.area.y, self.area.bottom() - 1),
        )
    }

    fn column_start(&self, mut x: u16, y: u16) -> u16 {
        while x > self.area.x && self.cells[self.index(x, y)].is_empty() {
            x -= 1;
        }
        x
    }

    fn index(&self, x: u16, y: u16) -> usize {
        usize::from(y - self.area.y) * usize::from(self.area.width) + usize::from(x - self.area.x)
    }

    fn row_bounds(&self, range: &Range, y: u16) -> (u16, u16) {
        let (start, end) = range.ordered();
        let left = if y == start.y { start.x } else { self.area.x };
        let right = if y == end.y {
            end.x
        } else {
            self.area.right() - 1
        };
        (self.column_start(left, y), self.column_start(right, y))
    }

    fn text(&self, range: &Range) -> String {
        let (start, end) = range.ordered();
        let mut rows = Vec::new();
        for y in start.y..=end.y {
            let (left, right) = self.row_bounds(range, y);
            let mut row = String::new();
            for x in left..=right {
                row.push_str(&self.cells[self.index(x, y)]);
            }
            row.truncate(row.trim_end().len());
            rows.push(row);
        }
        rows.join("\n")
    }
}

#[derive(Debug)]
struct Range {
    pane: usize,
    anchor: Position,
    end: Position,
    dragged: bool,
}

impl Range {
    fn ordered(&self) -> (Position, Position) {
        if (self.anchor.y, self.anchor.x) <= (self.end.y, self.end.x) {
            (self.anchor, self.end)
        } else {
            (self.end, self.anchor)
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct Selection {
    panes: [Option<Snapshot>; 2],
    range: Option<Range>,
    clipboard: Option<String>,
}

impl Selection {
    pub(super) fn clear(&mut self) -> bool {
        self.range.take().is_some()
    }

    pub(super) fn reset(&mut self) {
        self.clear();
        self.panes = [None, None];
    }

    pub(super) fn begin(&mut self, x: u16, y: u16) {
        self.clear();
        for (pane, snapshot) in self.panes.iter().enumerate() {
            if snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.area.contains((x, y).into()))
            {
                self.range = Some(Range {
                    pane,
                    anchor: Position::new(x, y),
                    end: Position::new(x, y),
                    dragged: false,
                });
                break;
            }
        }
    }

    pub(super) fn drag(&mut self, x: u16, y: u16) -> bool {
        let Some(range) = &mut self.range else {
            return false;
        };
        let Some(snapshot) = &self.panes[range.pane] else {
            return false;
        };
        range.end = snapshot.clamp(x, y);
        range.dragged |= range.end != range.anchor;
        range.dragged
    }

    pub(super) fn release(&mut self, x: u16, y: u16) -> Option<Position> {
        self.drag(x, y);
        let range = self.range.as_ref()?;
        if range.dragged {
            return None;
        }
        let point = range.anchor;
        self.clear();
        Some(point)
    }

    pub(super) fn copy(&mut self) -> bool {
        let Some(range) = self.range.as_ref().filter(|range| range.dragged) else {
            return false;
        };
        let Some(snapshot) = &self.panes[range.pane] else {
            return false;
        };
        let text = snapshot.text(range);
        // Terminal output enforces the final byte limit, including newlines.
        if !text.is_empty() {
            self.clipboard = Some(text);
        }
        self.clear();
        true
    }

    pub(super) fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard.take()
    }

    pub(super) fn render(&mut self, buffer: &mut Buffer, areas: [Option<Rect>; 2]) {
        let panes = areas.map(|area| area.and_then(|area| Snapshot::capture(buffer, area)));
        if let Some(range) = &self.range {
            let unchanged = self.panes[range.pane]
                .as_ref()
                .zip(panes[range.pane].as_ref())
                .is_some_and(|(old, new)| {
                    old.area == new.area && old.text(range) == new.text(range)
                });
            if !unchanged {
                self.clear();
            }
        }
        self.panes = panes;
        let Some(range) = self.range.as_ref().filter(|range| range.dragged) else {
            return;
        };
        let Some(snapshot) = &self.panes[range.pane] else {
            return;
        };
        let (start, end) = range.ordered();
        for y in start.y..=end.y {
            let (left, right) = snapshot.row_bounds(range, y);
            let width = UnicodeWidthStr::width(snapshot.cells[snapshot.index(right, y)].as_str())
                .max(1) as u16;
            for x in left..right.saturating_add(width).min(snapshot.area.right()) {
                buffer[(x, y)]
                    .set_bg(SELECTED_BACKGROUND)
                    .set_fg(crate::theme::STRONG_FOREGROUND);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(text: &str) -> Buffer {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 16, 3));
        buffer.set_string(1, 1, text, ratatui::style::Style::default());
        buffer
    }

    #[test]
    fn reverse_drag_copies_complete_wide_and_combining_graphemes() {
        let mut selection = Selection::default();
        let mut buffer = frame("A界e\u{301}🌸Z");
        let area = Some(Rect::new(1, 1, 12, 1));
        selection.render(&mut buffer, [area, None]);
        selection.begin(6, 1);
        assert!(selection.drag(3, 1));
        selection.release(3, 1);
        selection.render(&mut buffer, [area, None]);
        assert_eq!(buffer[(2, 1)].bg, SELECTED_BACKGROUND);
        assert_eq!(buffer[(3, 1)].bg, SELECTED_BACKGROUND);
        assert_eq!(buffer[(5, 1)].bg, SELECTED_BACKGROUND);
        assert_eq!(buffer[(6, 1)].bg, SELECTED_BACKGROUND);
        assert!(selection.copy());
        assert_eq!(selection.take_clipboard().as_deref(), Some("界e\u{301}🌸"));
        assert!(!selection.copy());
    }

    #[test]
    fn changes_outside_the_selection_preserve_it_and_selected_changes_clear_it() {
        let mut selection = Selection::default();
        let area = Some(Rect::new(1, 1, 12, 1));
        selection.render(&mut frame("hello"), [area, None]);
        selection.begin(1, 1);
        selection.drag(4, 1);
        selection.render(&mut frame("hello more"), [area, None]);
        assert!(selection.copy());
        assert_eq!(selection.take_clipboard().as_deref(), Some("hell"));
        selection.begin(1, 1);
        selection.drag(4, 1);
        selection.render(&mut frame("other"), [area, None]);
        assert!(!selection.copy());
    }

    #[test]
    fn resize_reset_and_overlarge_snapshots_clear_selection_without_copying() {
        let mut selection = Selection::default();
        let area = Some(Rect::new(1, 1, 12, 1));
        selection.render(&mut frame("hello"), [area, None]);
        selection.begin(1, 1);
        selection.drag(4, 1);
        selection.render(&mut frame("hello"), [Some(Rect::new(1, 1, 11, 1)), None]);
        assert!(!selection.copy());
        assert!(
            Snapshot::capture(
                &Buffer::empty(Rect::new(0, 0, 300, 120)),
                Rect::new(0, 0, 300, 120)
            )
            .is_none()
        );
        selection.render(&mut frame("hello"), [area, None]);
        selection.begin(1, 1);
        selection.drag(4, 1);
        selection.reset();
        assert!(!selection.copy());
    }

    #[test]
    fn click_is_released_once_and_drag_stays_in_its_origin_pane() {
        let mut selection = Selection::default();
        let area = Some(Rect::new(1, 1, 5, 1));
        selection.render(&mut frame("hello"), [area, None]);
        selection.begin(1, 1);
        assert_eq!(selection.release(1, 1), Some(Position::new(1, 1)));
        assert_eq!(selection.release(1, 1), None);
        selection.begin(1, 1);
        selection.drag(100, 100);
        assert_eq!(selection.release(100, 100), None);
        assert!(selection.copy());
        assert_eq!(selection.take_clipboard().as_deref(), Some("hello"));
    }
}
