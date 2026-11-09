//! Bounded terminal text layout shared by the CommonMark renderer and source view.
use crate::editor::{MAX_VISUAL_LINE_BYTES, display_grapheme};
use ratatui::{
    style::Style,
    text::{Line, Span},
};
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MAX_SPANS: usize = 256;
mod document;
mod math;
mod mermaid;
mod syntax;
mod table;
#[cfg(test)]
pub(crate) use document::FootnoteAnchor;
pub(crate) use document::{RenderedDocument, render_document};

fn append_span(spans: &mut Vec<Span<'static>>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    let full = spans.len() >= MAX_SPANS;
    if let Some(last) = spans.last_mut()
        && (last.style == style || full)
    {
        last.content.to_mut().push_str(text);
    } else {
        spans.push(Span::styled(text.to_owned(), style));
    }
}

/// Wrap a sanitized line without splitting graphemes, including across spans.
/// The first source byte determines a grapheme's style. Retain at most `limit`
/// tail rows while counting every row, with a minimum effective width of one.
/// Oversized clusters become an ellipsis, and byte-limited rows show continuation.
pub(crate) fn append_wrapped_line(
    line: Line<'static>,
    width: usize,
    lines: &mut VecDeque<Line<'static>>,
    count: &mut usize,
    limit: usize,
) {
    append_wrapped_line_observed(line, width, lines, count, limit, |_| {});
}

struct WrappedPosition {
    source_offset: usize,
    row: usize,
    byte_offset: usize,
    bytes: usize,
}

fn append_wrapped_line_observed(
    line: Line<'static>,
    width: usize,
    lines: &mut VecDeque<Line<'static>>,
    count: &mut usize,
    limit: usize,
    mut observe: impl FnMut(WrappedPosition),
) {
    let width = width.max(1);
    let text = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    let mut sources = line.spans.iter();
    let mut source_end = 0;
    let mut style = line.style;
    let mut current = Vec::new();
    let mut column: usize = 0;
    let mut bytes: usize = 0;
    for (offset, grapheme) in text.grapheme_indices(true) {
        while offset >= source_end {
            let Some(source) = sources.next() else { break };
            source_end += source.content.len();
            style = line.style.patch(source.style);
        }
        if grapheme == "\n" {
            push_line(std::mem::take(&mut current), &line, lines, count, limit);
            column = 0;
            bytes = 0;
            continue;
        }
        let grapheme = display_grapheme(grapheme);
        let cells = UnicodeWidthStr::width(grapheme);
        let rendered = if cells > width { "�" } else { grapheme };
        if bytes.saturating_add(rendered.len()) > MAX_VISUAL_LINE_BYTES {
            push_line(std::mem::take(&mut current), &line, lines, count, limit);
            append_span(&mut current, "…", style);
            column = 1;
            bytes = "…".len();
        }
        if column > 0 && column.saturating_add(cells) > width {
            push_line(std::mem::take(&mut current), &line, lines, count, limit);
            column = 0;
            bytes = 0;
        }
        observe(WrappedPosition {
            source_offset: offset,
            row: *count,
            byte_offset: bytes,
            bytes: rendered.len(),
        });
        append_span(&mut current, rendered, style);
        bytes += rendered.len();
        column = column.saturating_add(if cells > width { 1 } else { cells });
    }
    push_line(current, &line, lines, count, limit);
}

fn push_line(
    spans: Vec<Span<'static>>,
    source: &Line<'_>,
    lines: &mut VecDeque<Line<'static>>,
    count: &mut usize,
    limit: usize,
) {
    *count = count.saturating_add(1);
    if limit == 0 {
        lines.clear();
        return;
    }
    while lines.len() >= limit {
        lines.pop_front();
    }
    lines.push_back(Line {
        spans,
        style: source.style,
        alignment: source.alignment,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{AMBER, CODE_BACKGROUND, FOREGROUND, HEADING, INLINE_CODE};
    use ratatui::style::Color;
    fn content(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }
    #[test]
    fn wrapping_combines_graphemes_across_spans_and_inherits_line_background() {
        let base = Style::default().fg(FOREGROUND).bg(CODE_BACKGROUND);
        let line = Line::from(vec![
            Span::styled("e", Style::default().fg(AMBER)),
            Span::styled("\u{301}👩", Style::default().fg(HEADING)),
            Span::styled("\u{200d}💻中文", Style::default().fg(INLINE_CODE)),
        ])
        .style(base);
        let mut lines = VecDeque::new();
        let mut count = 0;
        append_wrapped_line(line, 2, &mut lines, &mut count, 10);
        assert_eq!(count, 4);
        assert_eq!(
            lines.iter().map(content).collect::<Vec<_>>(),
            ["e\u{301}", "👩\u{200d}💻", "中", "文"]
        );
        assert_eq!(lines[0].spans[0].style.fg, Some(AMBER));
        assert_eq!(lines[1].spans[0].style.fg, Some(HEADING));
        assert_eq!(lines[2].spans[0].style.fg, Some(INLINE_CODE));
        assert!(
            lines
                .iter()
                .all(|line| line.style.bg == Some(CODE_BACKGROUND))
        );
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.style.bg == Some(CODE_BACKGROUND))
        );
    }

    #[test]
    fn wrapping_preserves_styles_across_rows_and_counts_evicted_rows() {
        let line = Line::from(vec![
            Span::styled("abcdef", Style::default().fg(AMBER)),
            Span::styled("ghij", Style::default().fg(INLINE_CODE)),
        ]);
        let mut lines = VecDeque::new();
        let mut count = 0;
        append_wrapped_line(line, 3, &mut lines, &mut count, 2);
        assert_eq!(count, 4);
        assert_eq!(lines.iter().map(content).collect::<Vec<_>>(), ["ghi", "j"]);
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.style.fg == Some(INLINE_CODE))
        );
        append_wrapped_line(Line::raw("next"), 3, &mut lines, &mut count, 2);
        assert_eq!(count, 6);
        assert_eq!(lines.iter().map(content).collect::<Vec<_>>(), ["nex", "t"]);
    }

    #[test]
    fn empty_zero_width_zero_budget_and_saturating_count_are_safe() {
        let mut lines = VecDeque::new();
        let mut count = 0;
        append_wrapped_line(
            Line::from(Span::raw("中\nx\n")),
            0,
            &mut lines,
            &mut count,
            10,
        );
        assert_eq!(count, 3);
        assert_eq!(
            lines.iter().map(content).collect::<Vec<_>>(),
            ["�", "x", ""]
        );
        append_wrapped_line(Line::default(), 1, &mut lines, &mut count, 0);
        assert_eq!(count, 4);
        assert!(lines.is_empty());
        count = usize::MAX;
        append_wrapped_line(Line::raw("x"), usize::MAX, &mut lines, &mut count, 1);
        assert_eq!(count, usize::MAX);
        assert_eq!(content(&lines[0]), "x");
    }

    #[test]
    fn long_lines_keep_a_bounded_cache_and_coalesce_equal_styles() {
        let text = "x".repeat(256 * 1024);
        let mut lines = VecDeque::new();
        let mut count = 0;
        append_wrapped_line(
            Line::styled(text, Style::default().fg(AMBER)),
            80,
            &mut lines,
            &mut count,
            3,
        );
        assert_eq!(count, 3277);
        assert_eq!(lines.len(), 3);
        assert_eq!(content(lines.back().unwrap()).len(), 64);
        assert!(lines.iter().all(|line| line.spans.len() == 1));
        assert!(
            lines
                .iter()
                .all(|line| line.spans[0].style.fg == Some(AMBER))
        );
    }

    #[test]
    fn adversarial_span_counts_fall_back_to_literal_text_without_unbounded_allocations() {
        let source = Line::from(
            (0..10_000)
                .map(|index| {
                    Span::styled(
                        "x",
                        Style::default().fg(if index % 2 == 0 {
                            Color::Red
                        } else {
                            Color::Blue
                        }),
                    )
                })
                .collect::<Vec<_>>(),
        );
        let mut lines = VecDeque::new();
        let mut count = 0;
        append_wrapped_line(source, usize::MAX, &mut lines, &mut count, 3);
        assert_eq!(count, 3);
        assert!(
            lines
                .iter()
                .all(|line| content(line).len() <= MAX_VISUAL_LINE_BYTES)
        );
        assert_eq!(
            lines
                .iter()
                .map(content)
                .collect::<String>()
                .replace('…', ""),
            "x".repeat(10_000)
        );
        assert!(lines.iter().all(|line| line.spans.len() <= MAX_SPANS));
    }

    #[test]
    fn oversized_clusters_and_zero_width_runs_have_bounded_styled_rows() {
        let cluster = format!("a{}", "\u{301}".repeat(100_000));
        let line = Line::from(vec![
            Span::styled("a", Style::default().fg(AMBER)),
            Span::styled(cluster[1..].to_owned(), Style::default().fg(HEADING)),
            Span::raw(" tail"),
        ])
        .style(Style::default().bg(CODE_BACKGROUND));
        let mut lines = VecDeque::new();
        let mut count = 0;
        append_wrapped_line(line, 80, &mut lines, &mut count, 3);
        assert_eq!(content(&lines[0]), "… tail");
        assert_eq!(lines[0].spans[0].style.fg, Some(AMBER));
        assert_eq!(lines[0].style.bg, Some(CODE_BACKGROUND));

        let text = format!("a{}tail", "\u{200b}".repeat(100_000));
        lines.clear();
        count = 0;
        append_wrapped_line(Line::raw(text), 80, &mut lines, &mut count, 3);
        assert!(count > 3);
        assert_eq!(lines.len(), 3);
        assert!(
            lines
                .iter()
                .all(|line| content(line).len() <= MAX_VISUAL_LINE_BYTES)
        );
        assert!(content(lines.back().unwrap()).ends_with("tail"));
        assert!(lines.iter().any(|line| content(line).starts_with('…')));
    }
}
