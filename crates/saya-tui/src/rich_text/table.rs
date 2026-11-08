//! Width-aware pipe tables with bounded row buffering.
use super::{append_span, append_wrapped_line, inline_spans};
use crate::editor::display_grapheme;
use crate::theme::{BORDER, CODE_BACKGROUND, FOREGROUND, MUTED};
use ratatui::{
    layout::Alignment,
    style::{Modifier, Style},
    text::{Line, Span},
};
use std::{collections::VecDeque, iter::Peekable, str::Split};
use unicode_segmentation::UnicodeSegmentation;

const MAX_COLUMNS: usize = 32;
const WIDTH_SAMPLE_ROWS: usize = 128;
const MAX_CELL_LINES: usize = 128;
const MAX_TABLE_LINES: usize = 10_000;

pub(super) struct Table {
    headers: Vec<String>,
    field_labels: Vec<Line<'static>>,
    alignments: Vec<Alignment>,
    widths: Option<Vec<usize>>,
    width: usize,
    pending: VecDeque<Line<'static>>,
    finished: bool,
    rendered_lines: usize,
}

impl Table {
    pub(super) fn new<'a>(
        header: &str,
        separator: &str,
        following: impl Iterator<Item = &'a str>,
        width: usize,
    ) -> Option<Self> {
        if !header.contains('|') {
            return None;
        }
        let (headers, separated) = cells(header)?;
        if !separated {
            return None;
        }
        let (separators, _) = cells(separator)?;
        if separators.len() != headers.len() {
            return None;
        }
        let alignments = separators
            .iter()
            .map(|cell| {
                let rule = cell.strip_prefix(':').unwrap_or(cell);
                let rule = rule.strip_suffix(':').unwrap_or(rule);
                if rule.len() < 3 || !rule.bytes().all(|byte| byte == b'-') {
                    return None;
                }
                Some(match (cell.starts_with(':'), cell.ends_with(':')) {
                    (true, true) => Alignment::Center,
                    (false, true) => Alignment::Right,
                    _ => Alignment::Left,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let width = width.max(1);
        let mut widths = headers
            .iter()
            .map(|cell| cell_line(cell, true).width().clamp(4, 48))
            .collect::<Vec<_>>();
        for line in following.take(WIDTH_SAMPLE_ROWS) {
            let Some(row) = body_cells(line, headers.len()) else {
                break;
            };
            for (column, cell) in row.iter().enumerate() {
                widths[column] = widths[column].max(cell_line(cell, false).width().min(48));
            }
        }
        let overhead = headers.len() * 3 + 1;
        let widths = if width < overhead + headers.len() * 4 {
            None
        } else {
            let available = width - overhead;
            while widths.iter().sum::<usize>() > available {
                let (index, _) = widths.iter().enumerate().max_by_key(|(_, value)| **value)?;
                widths[index] -= 1;
            }
            Some(widths)
        };
        let field_labels = headers
            .iter()
            .map(|header| {
                let mut clusters = header.graphemes(true);
                let mut label = clusters
                    .by_ref()
                    .take(80)
                    .map(display_grapheme)
                    .collect::<String>();
                if clusters.next().is_some() {
                    label.push('…');
                }
                cell_line(&label, true)
            })
            .collect();
        let mut table = Self {
            headers,
            field_labels,
            alignments,
            widths,
            width,
            pending: VecDeque::new(),
            finished: false,
            rendered_lines: 0,
        };
        if table.widths.is_some() {
            table.pending.push_back(table.border('┌', '┬', '┐'));
            let header = table.headers.clone();
            table.render_row(&header, true);
            table.pending.push_back(table.border('├', '┼', '┤'));
        } else {
            let header = table.headers.clone();
            for cell in header {
                table
                    .pending
                    .extend(wrap_cell(cell_line(&cell, true), width));
            }
        }
        Some(table)
    }

    pub(super) fn next(&mut self, source: &mut Peekable<Split<'_, char>>) -> Option<Line<'static>> {
        if self.rendered_lines == MAX_TABLE_LINES {
            self.pending.clear();
            self.finished = true;
            self.rendered_lines += 1;
            return Some(Line::styled(
                "[table display truncated]",
                Style::default().fg(MUTED),
            ));
        }
        self.rendered_lines += 1;
        if let Some(line) = self.pending.pop_front() {
            return Some(line);
        }
        if self.finished {
            return None;
        }
        if let Some(row) = source
            .peek()
            .and_then(|line| body_cells(line, self.headers.len()))
        {
            source.next();
            self.render_row(&row, false);
            return self.pending.pop_front();
        }
        self.finished = true;
        self.widths.as_ref().map(|_| self.border('└', '┴', '┘'))
    }

    fn border(&self, left: char, middle: char, right: char) -> Line<'static> {
        let segments = self
            .widths
            .as_ref()
            .into_iter()
            .flatten()
            .map(|width| "─".repeat(width + 2))
            .collect::<Vec<_>>();
        Line::styled(
            format!("{left}{}{right}", segments.join(&middle.to_string())),
            Style::default().fg(BORDER),
        )
    }

    fn render_row(&mut self, row: &[String], header: bool) {
        let Some(widths) = &self.widths else {
            self.pending.push_back(Line::styled(
                "─".repeat(self.width.min(32)),
                Style::default().fg(BORDER),
            ));
            for (index, cell) in row.iter().enumerate() {
                let mut line = self.field_labels[index].clone();
                line.spans.push(Span::raw("  "));
                line.spans.extend(cell_line(cell, false).spans);
                self.pending.extend(wrap_cell(line, self.width));
            }
            return;
        };
        let wrapped = row
            .iter()
            .zip(widths)
            .map(|(cell, width)| wrap_cell(cell_line(cell, header), *width))
            .collect::<Vec<_>>();
        let height = wrapped.iter().map(VecDeque::len).max().unwrap_or(1);
        let base = if header {
            Style::default().fg(FOREGROUND).bg(CODE_BACKGROUND)
        } else {
            Style::default().fg(FOREGROUND)
        };
        let border = base.fg(BORDER);
        for index in 0..height {
            let mut spans = Vec::new();
            append_span(&mut spans, "│", border);
            for (column, width) in widths.iter().enumerate() {
                let line = wrapped[column].get(index);
                let padding = width.saturating_sub(line.map_or(0, Line::width));
                let leading = match self.alignments[column] {
                    Alignment::Right => padding,
                    Alignment::Center => padding / 2,
                    Alignment::Left => 0,
                };
                append_span(&mut spans, &" ".repeat(leading + 1), base);
                if let Some(line) = line {
                    for span in &line.spans {
                        append_span(&mut spans, &span.content, base.patch(span.style));
                    }
                }
                append_span(&mut spans, &" ".repeat(padding - leading + 1), base);
                append_span(&mut spans, "│", border);
            }
            self.pending.push_back(Line::from(spans).style(base));
        }
    }
}

fn body_cells(line: &str, columns: usize) -> Option<Vec<String>> {
    if !line.contains('|') {
        return None;
    }
    let (mut row, separated) = cells(line)?;
    if !separated {
        return None;
    }
    if row.len() > columns {
        return None;
    }
    row.resize(columns, String::new());
    Some(row)
}

fn cells(line: &str) -> Option<(Vec<String>, bool)> {
    if line.len() - line.trim_start_matches(' ').len() > 3 || line.trim().is_empty() {
        return None;
    }
    let line = line.trim();
    let mut cells = vec![String::new()];
    let mut characters = line.chars().peekable();
    let mut trailing_separator = false;
    let mut separated = false;
    while let Some(character) = characters.next() {
        trailing_separator = false;
        match character {
            '\\' if characters.peek() == Some(&'|') => {
                characters.next();
                cells.last_mut()?.push('|');
            }
            '\\' if characters.peek() == Some(&'\\') => {
                characters.next();
                cells.last_mut()?.push_str("\\\\");
            }
            '|' => {
                if cells.len() >= MAX_COLUMNS + 2 {
                    return None;
                }
                cells.push(String::new());
                trailing_separator = true;
                separated = true;
            }
            _ => cells.last_mut()?.push(character),
        }
    }
    if trailing_separator {
        cells.pop();
    }
    if line.starts_with('|') {
        cells.remove(0);
    }
    if cells.is_empty() || cells.len() > MAX_COLUMNS {
        return None;
    }
    Some((
        cells
            .into_iter()
            .map(|cell| cell.trim().to_owned())
            .collect(),
        separated,
    ))
}

fn cell_line(cell: &str, header: bool) -> Line<'static> {
    let mut spans = Vec::new();
    let base = if header {
        Style::default().fg(FOREGROUND).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(FOREGROUND)
    };
    inline_spans(cell, base, &mut spans, 0);
    Line::from(spans).style(base)
}

fn wrap_cell(line: Line<'static>, width: usize) -> VecDeque<Line<'static>> {
    let mut rows = VecDeque::new();
    let mut count = 0;
    append_wrapped_line(line, width, &mut rows, &mut count, MAX_CELL_LINES);
    if count > MAX_CELL_LINES {
        rows.pop_front();
        rows.push_front(Line::styled(
            if width >= 24 {
                "[cell display truncated]"
            } else {
                "…"
            },
            Style::default().fg(MUTED),
        ));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{editor::safe_text, rich_text::markdown_lines_with_width};

    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn grid_aligns_columns_using_display_width_and_keeps_cell_styles() {
        let source = "| Name | 中 | Count |\n| :--- | :---: | ---: |\n| **加粗** | e\u{301} | 7 |\n| plain | 中文 | 123 |";
        let lines = markdown_lines_with_width(source, 50).collect::<Vec<_>>();
        assert_eq!(lines.len(), 6);
        assert!(text(&lines[0]).starts_with('┌'));
        assert!(text(&lines[5]).ends_with('┘'));
        assert!(lines.iter().all(|line| line.width() == lines[0].width()));
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|span| span.content.contains("Name")
                    && span.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(
            lines[3]
                .spans
                .iter()
                .any(|span| span.content.contains("加粗")
                    && span.style.add_modifier.contains(Modifier::BOLD))
        );
        let short = text(&lines[3]);
        let long = text(&lines[4]);
        assert_eq!(
            short.split('│').nth(3).unwrap().trim_end().len(),
            long.split('│').nth(3).unwrap().trim_end().len()
        );
    }

    #[test]
    fn escaped_pipes_optional_borders_and_missing_cells_are_supported() {
        let source = "Name | Detail\n--- | ---\nA | `left\\|right`\n| B |\n| C | final |\n\nafter";
        let rendered = markdown_lines_with_width(source, 60)
            .map(|line| text(&line))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("left|right"));
        assert!(rendered.contains("│ B    │"));
        assert!(rendered.ends_with("\n\nafter"));
        assert_eq!(
            cells(r"| one\\ | two\|three |"),
            Some((vec![r"one\\".into(), "two|three".into()], true))
        );
    }

    #[test]
    fn invalid_rules_and_extra_cells_fall_back_without_dropping_text() {
        for source in [
            "A | B\n---\nfirst | second",
            "A | B\n:---:: | ---\nfirst | second",
            "A | B\n-- | ---\nfirst | second",
        ] {
            let rendered = markdown_lines_with_width(source, 80)
                .map(|line| text(&line))
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(rendered, source);
        }
        let source = "A | B\n--- | ---\none | two | extra\nafter";
        let rendered = markdown_lines_with_width(source, 80)
            .map(|line| text(&line))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.ends_with("one | two | extra\nafter"));
        let source = "foo\\|bar\n---\nvalue |";
        let rendered = markdown_lines_with_width(source, 80)
            .map(|line| text(&line))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(rendered, "foo|bar\n---\nvalue |");
    }

    #[test]
    fn fences_take_precedence_over_tables_and_inline_code_headers_work() {
        let fenced = "```text | literal\n--- | ---\n**raw** | cell\n```";
        let lines = markdown_lines_with_width(fenced, 80).collect::<Vec<_>>();
        assert_eq!(text(&lines[0]), "╭─ text | literal");
        assert_eq!(text(&lines[1]), "--- | ---");
        assert_eq!(text(&lines[2]), "**raw** | cell");
        assert_eq!(text(&lines[3]), "╰─");
        let lines =
            markdown_lines_with_width("`Name` | Detail\n--- | ---\nA | B", 80).collect::<Vec<_>>();
        assert!(text(&lines[0]).starts_with('┌'));
        assert!(text(&lines[1]).contains("Name"));
    }

    #[test]
    fn narrow_tables_preserve_values_and_sanitized_controls() {
        let source = safe_text(
            "| Name | Detail | State |\n| --- | --- | --- |\n| 中文 | e\u{301}\u{1b}[2J | Done |",
        );
        let lines = markdown_lines_with_width(&source, 20).collect::<Vec<_>>();
        assert!(lines.iter().all(|line| line.width() <= 20));
        let rendered = lines.iter().map(text).collect::<Vec<_>>().join("\n");
        assert!(rendered.contains("中文"));
        assert!(rendered.contains("e\u{301}\\u{1b}[2J"));
        assert!(rendered.contains("State  Done"));
        assert!(
            !rendered
                .chars()
                .any(|character| character.is_control() && character != '\n')
        );
    }

    #[test]
    fn large_cells_columns_and_table_expansion_have_visible_budgets() {
        let source = format!("| Name |\n| --- |\n| {} END |", "x".repeat(20_000));
        let lines = markdown_lines_with_width(&source, 40).collect::<Vec<_>>();
        assert!(lines.len() <= MAX_CELL_LINES + 4);
        let rendered = lines.iter().map(text).collect::<Vec<_>>().join("\n");
        assert!(rendered.contains("[cell display truncated]"));
        assert!(rendered.contains("END"));
        let source = format!(
            "{}\n{}",
            vec!["column"; MAX_COLUMNS + 1].join(" | "),
            vec!["---"; MAX_COLUMNS + 1].join(" | ")
        );
        assert_eq!(
            text(&markdown_lines_with_width(&source, 80).next().unwrap()),
            source.lines().next().unwrap()
        );
        let source = format!(
            "| Name |\n| --- |\n{}",
            "| item |\n".repeat(MAX_TABLE_LINES + 10)
        );
        let lines = markdown_lines_with_width(&source, 40).collect::<Vec<_>>();
        assert!(
            lines
                .iter()
                .any(|line| text(line) == "[table display truncated]")
        );
        assert!(lines.len() <= MAX_TABLE_LINES + 20);
    }
}
