//! Width-aware presentation of parsed table cells and semantic footnote anchors.
use super::document::{RichLine, wrap_line};
use crate::{
    editor::MAX_GRAPHEME_BYTES,
    theme::{BORDER, CODE_BACKGROUND, FOREGROUND, MUTED},
};
use pulldown_cmark::Alignment;
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;

const MAX_COLUMNS: usize = 32;
const MAX_CELLS: usize = 4096;
const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_ANCHORS: usize = 4096;
const WIDTH_SAMPLE_ROWS: usize = 128;
const MAX_CELL_LINES: usize = 128;
const MAX_TABLE_LINES: usize = 10_000;
const MAX_SPANS: usize = 256;

/// Lay out parsed cells. Invalid dimensions or excessive input return to source.
pub(super) fn render(
    headers: Vec<RichLine>,
    mut rows: Vec<Vec<RichLine>>,
    alignments: &[Alignment],
    width: usize,
) -> Option<Vec<RichLine>> {
    let columns = headers.len();
    if columns == 0 || columns > MAX_COLUMNS || alignments.len() != columns {
        return None;
    }
    let mut cell_count = columns;
    let mut bytes = 0usize;
    let mut anchors = 0usize;
    for row in &mut rows {
        if row.len() > columns {
            return None;
        }
        cell_count = cell_count.checked_add(columns)?;
        if cell_count > MAX_CELLS {
            return None;
        }
        row.resize_with(columns, RichLine::default);
    }
    for cell in headers.iter().chain(rows.iter().flatten()) {
        let length = byte_length(cell);
        bytes = bytes.checked_add(length)?;
        anchors = anchors.checked_add(cell.anchors.len())?;
        for anchor in &cell.anchors {
            if anchor.offset > length || anchor.length > length - anchor.offset {
                return None;
            }
            bytes = bytes.checked_add(anchor.key.len())?;
        }
        if bytes > MAX_INPUT_BYTES || anchors > MAX_ANCHORS {
            return None;
        }
    }
    let width = width.max(1);
    let mut widths = headers
        .iter()
        .map(|header| header.line.width().clamp(4, 48))
        .collect::<Vec<_>>();
    for row in rows.iter().take(WIDTH_SAMPLE_ROWS) {
        for (column, cell) in row.iter().enumerate() {
            widths[column] = widths[column].max(cell.line.width().min(48));
        }
    }
    let overhead = columns * 3 + 1;
    let mut output = Vec::new();
    if width < overhead + columns * 4 {
        let labels = headers.iter().map(field_label).collect::<Vec<_>>();
        for mut header in headers {
            header.line.style = header.line.style.add_modifier(Modifier::BOLD);
            for span in &mut header.line.spans {
                span.style = span.style.add_modifier(Modifier::BOLD);
            }
            if !extend(&mut output, wrapped(header, width)) {
                return Some(output);
            }
        }
        for row in rows {
            if !push(&mut output, plain("─".repeat(width.min(32)), BORDER)) {
                return Some(output);
            }
            for (column, cell) in row.into_iter().enumerate() {
                let mut line = labels[column].clone();
                text(&mut line, "  ", Style::default());
                append(&mut line, cell, Style::default().fg(FOREGROUND), false);
                if !extend(&mut output, wrapped(line, width)) {
                    return Some(output);
                }
            }
        }
        return Some(output);
    }
    let available = width - overhead;
    while widths.iter().sum::<usize>() > available {
        let (index, _) = widths.iter().enumerate().max_by_key(|(_, value)| **value)?;
        widths[index] -= 1;
    }
    push(&mut output, border(&widths, '┌', '┬', '┐'));
    if !extend(&mut output, grid_row(headers, &widths, alignments, true)) {
        return Some(output);
    }
    if !push(&mut output, border(&widths, '├', '┼', '┤')) {
        return Some(output);
    }
    for row in rows {
        if !extend(&mut output, grid_row(row, &widths, alignments, false)) {
            return Some(output);
        }
    }
    push(&mut output, border(&widths, '└', '┴', '┘'));
    Some(output)
}

fn grid_row(
    row: Vec<RichLine>,
    widths: &[usize],
    alignments: &[Alignment],
    header: bool,
) -> Vec<RichLine> {
    let wrapped = row
        .into_iter()
        .zip(widths)
        .map(|(cell, width)| wrapped(cell, *width))
        .collect::<Vec<_>>();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let base = if header {
        Style::default().fg(FOREGROUND).bg(CODE_BACKGROUND)
    } else {
        Style::default().fg(FOREGROUND)
    };
    let mut output = Vec::new();
    for index in 0..height {
        let mut line = RichLine {
            line: Line::default().style(base),
            anchors: Vec::new(),
        };
        text(&mut line, "│", base.fg(BORDER));
        for (column, width) in widths.iter().enumerate() {
            let cell = wrapped[column].get(index);
            let padding = width.saturating_sub(cell.map_or(0, |cell| cell.line.width()));
            let leading = match alignments[column] {
                Alignment::Right => padding,
                Alignment::Center => padding / 2,
                Alignment::Left | Alignment::None => 0,
            };
            text(&mut line, &" ".repeat(leading + 1), base);
            if let Some(cell) = cell {
                append(&mut line, cell.clone(), base, header);
            }
            text(&mut line, &" ".repeat(padding - leading + 1), base);
            text(&mut line, "│", base.fg(BORDER));
        }
        output.push(line);
    }
    output
}

fn field_label(header: &RichLine) -> RichLine {
    let content = header
        .line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    let mut end = 0;
    let mut truncated = false;
    for (count, (offset, grapheme)) in content.grapheme_indices(true).enumerate() {
        if count == 80 || grapheme.len() > MAX_GRAPHEME_BYTES {
            truncated = true;
            break;
        }
        end = offset + grapheme.len();
    }
    let mut output = RichLine {
        line: Line::default().style(header.line.style.add_modifier(Modifier::BOLD)),
        anchors: header
            .anchors
            .iter()
            .filter(|anchor| anchor.offset.saturating_add(anchor.length) <= end)
            .cloned()
            .collect(),
    };
    let mut remaining = end;
    for span in &header.line.spans {
        let length = remaining.min(span.content.len());
        // End is a grapheme boundary in the concatenated original spans.
        if length > 0 {
            text(
                &mut output,
                &span.content[..length],
                header
                    .line
                    .style
                    .patch(span.style)
                    .add_modifier(Modifier::BOLD),
            );
        }
        remaining -= length;
        if remaining == 0 {
            break;
        }
    }
    if truncated {
        text(&mut output, "…", Style::default().fg(MUTED));
    }
    output
}

fn wrapped(line: RichLine, width: usize) -> Vec<RichLine> {
    let mut rows = wrap_line(line, width, MAX_CELL_LINES + 1);
    if rows.len() > MAX_CELL_LINES {
        rows.remove(0);
        rows[0] = plain(
            if width >= 24 {
                "[cell display truncated]"
            } else {
                "…"
            },
            MUTED,
        );
    }
    rows
}

fn append(output: &mut RichLine, cell: RichLine, base: Style, header: bool) {
    let offset = byte_length(output);
    for span in cell.line.spans {
        let mut style = base.patch(cell.line.style).patch(span.style);
        if header {
            style = style.add_modifier(Modifier::BOLD)
        }
        text(output, &span.content, style);
    }
    output
        .anchors
        .extend(cell.anchors.into_iter().map(|mut anchor| {
            anchor.offset = anchor.offset.saturating_add(offset);
            anchor
        }));
}

fn text(output: &mut RichLine, content: &str, style: Style) {
    if content.is_empty() {
        return;
    }
    let full = output.line.spans.len() >= MAX_SPANS;
    if let Some(last) = output.line.spans.last_mut()
        && (last.style == style || full)
    {
        last.content.to_mut().push_str(content);
    } else {
        output
            .line
            .spans
            .push(Span::styled(content.to_owned(), style));
    }
}

fn byte_length(line: &RichLine) -> usize {
    line.line.spans.iter().map(|span| span.content.len()).sum()
}

fn plain(content: impl Into<String>, color: ratatui::style::Color) -> RichLine {
    RichLine {
        line: Line::styled(content.into(), Style::default().fg(color)),
        anchors: Vec::new(),
    }
}

fn border(widths: &[usize], left: char, middle: char, right: char) -> RichLine {
    let segments = widths
        .iter()
        .map(|width| "─".repeat(width + 2))
        .collect::<Vec<_>>();
    plain(
        format!("{left}{}{right}", segments.join(&middle.to_string())),
        BORDER,
    )
}

fn push(output: &mut Vec<RichLine>, line: RichLine) -> bool {
    if output.len() == MAX_TABLE_LINES - 1 {
        let marker = if output.last().is_some_and(|line| line.line.width() >= 24) {
            "[table display truncated]"
        } else {
            "…"
        };
        output.push(plain(marker, MUTED));
        return false;
    }
    output.push(line);
    true
}

fn extend(output: &mut Vec<RichLine>, lines: Vec<RichLine>) -> bool {
    for line in lines {
        if !push(output, line) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rich_text::document::ByteAnchor;
    use crate::theme::{AMBER, BLUE};

    fn cell(content: &str) -> RichLine {
        RichLine {
            line: Line::raw(content.to_owned()),
            anchors: Vec::new(),
        }
    }

    fn content(line: &RichLine) -> String {
        line.line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn reference(content: &str, marker: &str) -> RichLine {
        let offset = content.find(marker).unwrap();
        RichLine {
            line: Line::styled(content.to_owned(), Style::default().fg(BLUE)),
            anchors: vec![ByteAnchor {
                key: "semantic-key".into(),
                number: 7,
                offset,
                length: marker.len(),
                definition: false,
                occurrence: 12,
            }],
        }
    }

    fn check_anchors(lines: &[RichLine]) {
        for line in lines {
            let text = content(line);
            for anchor in &line.anchors {
                assert_eq!(anchor.key, "semantic-key");
                assert_eq!(anchor.number, 7);
                assert_eq!(anchor.occurrence, 12);
                assert!(!anchor.definition);
                let displayed = &text[anchor.offset..anchor.offset + anchor.length];
                assert!("[7]".contains(displayed));
            }
        }
    }

    #[test]
    fn grid_uses_display_width_alignment_and_original_cell_styles() {
        let headers = vec![cell("Name"), cell("中"), cell("Count")];
        let mut highlighted = cell("加粗");
        highlighted.line = highlighted
            .line
            .style(Style::default().fg(AMBER).add_modifier(Modifier::BOLD));
        let rows = vec![
            vec![highlighted, cell("e\u{301}"), cell("7")],
            vec![cell("plain"), cell("中文"), cell("123")],
        ];
        let output = render(
            headers,
            rows,
            &[Alignment::Left, Alignment::Center, Alignment::Right],
            50,
        )
        .unwrap();
        assert_eq!(output.len(), 6);
        assert!(
            output
                .iter()
                .all(|line| line.line.width() == output[0].line.width())
        );
        assert!(content(&output[0]).starts_with('┌'));
        assert!(content(&output[5]).ends_with('┘'));
        assert!(
            output[1]
                .line
                .spans
                .iter()
                .any(|span| span.content.contains("Name")
                    && span.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(
            output[3]
                .line
                .spans
                .iter()
                .any(|span| span.content.contains("加粗")
                    && span.style.fg == Some(AMBER)
                    && span.style.add_modifier.contains(Modifier::BOLD))
        );
        assert_eq!(
            content(&output[3])
                .split('│')
                .nth(3)
                .unwrap()
                .trim_end()
                .len(),
            content(&output[4])
                .split('│')
                .nth(3)
                .unwrap()
                .trim_end()
                .len()
        );
    }

    #[test]
    fn unicode_padding_moves_semantic_anchor_byte_offsets_without_guessing_labels() {
        let output = render(
            vec![cell("中文"), cell("Reference")],
            vec![vec![cell("e\u{301}"), reference("链接 [7]", "[7]")]],
            &[Alignment::Left, Alignment::Right],
            60,
        )
        .unwrap();
        check_anchors(&output);
        let line = output.iter().find(|line| !line.anchors.is_empty()).unwrap();
        assert_eq!(line.anchors.len(), 1);
        assert!(line.anchors[0].offset > line.line.width() / 2);
        assert!(content(line).contains("链接 [7]"));
        let plain_marker = render(
            vec![cell("Literal")],
            vec![vec![cell("[7]")]],
            &[Alignment::None],
            40,
        )
        .unwrap();
        assert!(plain_marker.iter().all(|line| line.anchors.is_empty()));
    }

    #[test]
    fn wrapped_and_narrow_fields_preserve_reference_metadata() {
        for width in [1, 20, 40] {
            let output = render(
                vec![cell("Name"), cell("Details"), cell("State")],
                vec![vec![
                    cell("中文"),
                    reference("some long text [7]", "[7]"),
                    cell("Done"),
                ]],
                &[Alignment::None; 3],
                width,
            )
            .unwrap();
            check_anchors(&output);
            assert!(output.iter().any(|line| !line.anchors.is_empty()));
            assert!(output.iter().all(|line| line.line.width() <= width));
        }
    }

    #[test]
    fn missing_cells_pad_and_invalid_dimensions_or_anchor_ranges_fail_closed() {
        let output = render(
            vec![cell("A"), cell("B")],
            vec![vec![cell("only")]],
            &[Alignment::None; 2],
            40,
        )
        .unwrap();
        assert!(output.iter().any(|line| content(line).contains("only")));
        assert!(render(Vec::new(), Vec::new(), &[], 40).is_none());
        assert!(
            render(
                vec![cell("A")],
                vec![vec![cell("one"), cell("extra")]],
                &[Alignment::None],
                40
            )
            .is_none()
        );
        assert!(render(vec![cell("A")], Vec::new(), &[], 40).is_none());
        assert!(
            render(
                vec![cell("A"); MAX_COLUMNS + 1],
                Vec::new(),
                &[Alignment::None; MAX_COLUMNS + 1],
                200
            )
            .is_none()
        );
        let mut bad = reference("[7]", "[7]");
        bad.anchors[0].offset = usize::MAX;
        assert!(render(vec![cell("A")], vec![vec![bad]], &[Alignment::None], 40).is_none());
    }

    #[test]
    fn cropped_labels_keep_complete_anchors_and_mark_omissions() {
        let header = reference(&format!("[7]{}", "中".repeat(90)), "[7]");
        let label = field_label(&header);
        assert!(content(&label).ends_with('…'));
        assert_eq!(label.anchors.len(), 1);
        check_anchors(&[label]);
        let header = reference(&format!("{}[7]", "x".repeat(79)), "[7]");
        assert!(field_label(&header).anchors.is_empty());
        let oversized = format!("e{}", "\u{301}".repeat(MAX_GRAPHEME_BYTES));
        assert_eq!(content(&field_label(&cell(&oversized))), "…");
    }

    #[test]
    fn cell_and_total_expansion_limits_remain_visible_and_bounded() {
        let large = format!("{} END", "x".repeat(20_000));
        let output = render(
            vec![cell("Name")],
            vec![vec![cell(&large)]],
            &[Alignment::None],
            40,
        )
        .unwrap();
        assert!(output.len() <= MAX_CELL_LINES + 4);
        assert!(
            output
                .iter()
                .any(|line| content(line).contains("[cell display truncated]"))
        );
        assert!(output.iter().any(|line| content(line).contains("END")));
        let output = render(
            vec![cell("A"), cell("B"), cell("C")],
            vec![vec![cell("x"); 3]; 1000],
            &[Alignment::None; 3],
            1,
        )
        .unwrap();
        assert!(output.len() <= MAX_TABLE_LINES);
        assert_eq!(content(output.last().unwrap()), "…");
        assert!(output.iter().all(|line| line.line.width() <= 1));
        assert!(
            render(
                vec![cell("A")],
                vec![vec![cell("x")]; MAX_CELLS],
                &[Alignment::None],
                40
            )
            .is_none()
        );
        assert!(
            render(
                vec![cell(&"x".repeat(MAX_INPUT_BYTES + 1))],
                Vec::new(),
                &[Alignment::None],
                40
            )
            .is_none()
        );
    }
}
