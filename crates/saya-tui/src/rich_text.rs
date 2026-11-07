//! A bounded presentation subset of Markdown, independent of runtime messages.
use crate::theme::{
    AMBER, BORDER, CODE_BACKGROUND, CODE_FOREGROUND, FOREGROUND, HEADING, INLINE_CODE, MUTED,
};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MAX_SPANS: usize = 256;
const MAX_INLINE_DEPTH: usize = 4;

#[derive(Clone, Copy)]
struct Fence {
    marker: u8,
    length: usize,
}

/// Render one logical line at a time. The caller must first apply `safe_text`.
/// HTML and URLs remain text, and unfinished inline delimiters stay visible.
pub(crate) fn markdown_lines(text: &str) -> impl Iterator<Item = Line<'static>> + '_ {
    let mut fence: Option<Fence> = None;
    text.split('\n').map(move |line| {
        let trimmed = line.trim_start_matches(' ');
        let indentation = line.len() - trimmed.len();
        let candidate = (indentation <= 3).then(|| fence_marker(trimmed)).flatten();
        if let Some(open) = fence {
            if let Some((close, rest)) = candidate
                && close.marker == open.marker
                && close.length >= open.length
                && rest.trim().is_empty()
            {
                fence = None;
                return Line::styled("╰─", Style::default().fg(BORDER).bg(CODE_BACKGROUND));
            }
            return Line::styled(
                line.to_owned(),
                Style::default().fg(CODE_FOREGROUND).bg(CODE_BACKGROUND),
            );
        }
        if let Some((open, language)) = candidate {
            fence = Some(open);
            let label = language.trim();
            let label = if label.is_empty() { "code" } else { label };
            let mut graphemes = label.graphemes(true);
            let mut label = graphemes.by_ref().take(80).collect::<String>();
            if graphemes.next().is_some() {
                label.push('…');
            }
            return Line::from(vec![
                Span::styled("╭─ ", Style::default().fg(BORDER)),
                Span::styled(label, Style::default().fg(MUTED)),
            ])
            .style(Style::default().bg(CODE_BACKGROUND));
        }
        let mut spans = Vec::new();
        let base = Style::default().fg(FOREGROUND);
        let heading_length = trimmed.bytes().take_while(|byte| *byte == b'#').count();
        if indentation <= 3
            && (1..=6).contains(&heading_length)
            && trimmed[heading_length..].starts_with(' ')
        {
            append_span(&mut spans, &line[..indentation], base);
            inline_spans(
                trimmed[heading_length..].trim_start_matches(' '),
                base.fg(HEADING).add_modifier(Modifier::BOLD),
                &mut spans,
                0,
            );
        } else if indentation <= 3 && (trimmed == ">" || trimmed.starts_with("> ")) {
            append_span(&mut spans, &line[..indentation], base);
            append_span(&mut spans, "│ ", base.fg(MUTED));
            inline_spans(
                trimmed[1..].trim_start_matches(' '),
                base.fg(MUTED),
                &mut spans,
                0,
            );
        } else if indentation <= 3
            && let Some(prefix_length) = list_prefix(trimmed)
        {
            append_span(&mut spans, &line[..indentation], base);
            append_span(&mut spans, &trimmed[..prefix_length], base.fg(AMBER));
            inline_spans(&trimmed[prefix_length..], base, &mut spans, 0);
        } else {
            inline_spans(line, base, &mut spans, 0);
        }
        Line::from(spans).style(base)
    })
}

fn fence_marker(text: &str) -> Option<(Fence, &str)> {
    let marker = *text.as_bytes().first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let length = text.bytes().take_while(|byte| *byte == marker).count();
    (length >= 3).then_some((Fence { marker, length }, &text[length..]))
}

fn list_prefix(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[0], b'-' | b'+' | b'*') && bytes[1] == b' ' {
        return Some(2);
    }
    let digits = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if (1..=9).contains(&digits)
        && matches!(bytes.get(digits), Some(b'.' | b')'))
        && bytes.get(digits + 1) == Some(&b' ')
    {
        Some(digits + 2)
    } else {
        None
    }
}

fn inline_spans(text: &str, base: Style, spans: &mut Vec<Span<'static>>, depth: usize) {
    if depth >= MAX_INLINE_DEPTH {
        append_span(spans, text, base);
        return;
    }
    let mut cursor = 0;
    let mut plain_start = 0;
    while cursor < text.len() && spans.len() < MAX_SPANS - 2 {
        if let Some(end) = literal_token_end(text, cursor) {
            cursor = end;
            continue;
        }
        let marker = text.as_bytes()[cursor];
        if !matches!(marker, b'*' | b'`') {
            cursor += text[cursor..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let length = text[cursor..]
            .bytes()
            .take_while(|byte| *byte == marker)
            .count();
        if length > 2 || (marker == b'`' && length != 1) {
            cursor += length;
            continue;
        }
        let content_start = cursor + length;
        if let Some(close) = closing_marker(text, content_start, marker, length)
            && close > content_start
        {
            append_span(spans, &text[plain_start..cursor], base);
            let content = &text[content_start..close];
            if marker == b'`' {
                append_span(spans, content, base.fg(INLINE_CODE).bg(CODE_BACKGROUND));
            } else {
                let modifier = if length == 2 {
                    Modifier::BOLD
                } else {
                    Modifier::ITALIC
                };
                inline_spans(
                    content,
                    base.fg(AMBER).add_modifier(modifier),
                    spans,
                    depth + 1,
                );
            }
            cursor = close + length;
            plain_start = cursor;
        } else {
            cursor += length;
        }
    }
    append_span(spans, &text[plain_start..], base);
}

fn closing_marker(text: &str, start: usize, marker: u8, length: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut cursor = start;
    while cursor < bytes.len() {
        if marker == b'*'
            && let Some(end) = literal_token_end(text, cursor)
        {
            cursor = end;
            continue;
        }
        if marker == b'*' && bytes[cursor] == b'`' {
            let run = bytes[cursor..]
                .iter()
                .take_while(|byte| **byte == b'`')
                .count();
            if run == 1
                && let Some(close) = closing_marker(text, cursor + 1, b'`', 1)
            {
                cursor = close + 1;
            } else {
                cursor += run;
            }
            continue;
        }
        if bytes[cursor] != marker {
            cursor += 1;
            continue;
        }
        let run = bytes[cursor..]
            .iter()
            .take_while(|byte| **byte == marker)
            .count();
        if run == length {
            return Some(cursor);
        }
        cursor += run;
    }
    None
}

fn literal_token_end(text: &str, start: usize) -> Option<usize> {
    let rest = &text.as_bytes()[start..];
    if rest.starts_with(b"https://") || rest.starts_with(b"http://") {
        let length = text[start..]
            .char_indices()
            .find(|(_, character)| character.is_whitespace() || matches!(character, '<' | '>'))
            .map_or(rest.len(), |(offset, _)| offset);
        return Some(start + length);
    }
    if rest.first() != Some(&b'<') {
        return None;
    }
    let name = if rest.get(1) == Some(&b'/') { 2 } else { 1 };
    if !rest
        .get(name)
        .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'!' | b'?'))
    {
        return None;
    }
    let mut quote = None;
    for (offset, byte) in rest.iter().copied().enumerate().skip(name + 1) {
        if quote == Some(byte) {
            quote = None;
        } else if quote.is_none() {
            match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'>' => return Some(start + offset + 1),
                _ => {}
            }
        }
    }
    // Incomplete tags remain literal through this logical line, keeping work linear.
    Some(text.len())
}

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
pub(crate) fn append_wrapped_line(
    line: Line<'static>,
    width: usize,
    lines: &mut VecDeque<Line<'static>>,
    count: &mut usize,
    limit: usize,
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
    for (offset, grapheme) in text.grapheme_indices(true) {
        while offset >= source_end {
            let Some(source) = sources.next() else { break };
            source_end += source.content.len();
            style = line.style.patch(source.style);
        }
        if grapheme == "\n" {
            push_line(std::mem::take(&mut current), &line, lines, count, limit);
            column = 0;
            continue;
        }
        let cells = UnicodeWidthStr::width(grapheme);
        if column > 0 && column.saturating_add(cells) > width {
            push_line(std::mem::take(&mut current), &line, lines, count, limit);
            column = 0;
        }
        let rendered = if cells > width { "�" } else { grapheme };
        append_span(&mut current, rendered, style);
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
    use crate::editor::safe_text;
    use ratatui::style::Color;

    fn content(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn headings_lists_and_quotes_use_semantic_styles_without_interpreting_html_or_urls() {
        let lines = markdown_lines(
            "## Heading\n- item\n12. ordered\n> quote\nhttps://example.test <b>HTML</b>",
        )
        .collect::<Vec<_>>();
        assert_eq!(content(&lines[0]), "Heading");
        assert_eq!(lines[0].spans[0].style.fg, Some(HEADING));
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(content(&lines[1]), "- item");
        assert_eq!(lines[1].spans[0].style.fg, Some(AMBER));
        assert_eq!(content(&lines[2]), "12. ordered");
        assert_eq!(lines[2].spans[0].style.fg, Some(AMBER));
        assert_eq!(content(&lines[3]), "│ quote");
        assert_eq!(lines[3].spans[0].style.fg, Some(MUTED));
        assert_eq!(content(&lines[4]), "https://example.test <b>HTML</b>");
    }

    #[test]
    fn url_tokens_and_html_tags_preserve_markers_even_inside_other_emphasis() {
        for text in [
            "https://example.test/a*b*c",
            "http://example.test/a`b`c",
            "https://example.test/**x**",
            "<tag attr='**x**' other=\"`y`\">",
            "<tag attr='x>y' other='*z*'>",
            "<tag unfinished **literal**",
        ] {
            assert_eq!(content(&markdown_lines(text).next().unwrap()), text);
        }
        let line =
            markdown_lines("**bold https://example.test/a*b*c done** <tag attr='**x**'> *after*")
                .next()
                .unwrap();
        assert_eq!(
            content(&line),
            "bold https://example.test/a*b*c done <tag attr='**x**'> after"
        );
        assert_eq!(line.spans[0].style.fg, Some(AMBER));
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
        let line = markdown_lines("**bold <tag attr='**x**'> done**")
            .next()
            .unwrap();
        assert_eq!(content(&line), "bold <tag attr='**x**'> done");
        assert_eq!(
            content(
                &markdown_lines("`**literal** <tag attr='x'> https://test/a*b*c`")
                    .next()
                    .unwrap()
            ),
            "**literal** <tag attr='x'> https://test/a*b*c"
        );
    }

    #[test]
    fn balanced_inline_markers_style_content_and_unfinished_markers_remain_literal() {
        let line = markdown_lines("plain **bold** *emphasis* `**code**` **nested `code`**")
            .next()
            .unwrap();
        assert_eq!(content(&line), "plain bold emphasis **code** nested code");
        let bold = line
            .spans
            .iter()
            .find(|span| span.content == "bold")
            .unwrap();
        assert_eq!(bold.style.fg, Some(AMBER));
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        let emphasis = line
            .spans
            .iter()
            .find(|span| span.content == "emphasis")
            .unwrap();
        assert!(emphasis.style.add_modifier.contains(Modifier::ITALIC));
        let code = line
            .spans
            .iter()
            .find(|span| span.content == "**code**")
            .unwrap();
        assert_eq!(code.style.fg, Some(INLINE_CODE));
        assert_eq!(code.style.bg, Some(CODE_BACKGROUND));
        assert_eq!(
            content(&markdown_lines("**a `**` b**").next().unwrap()),
            "a ** b"
        );
        for partial in [
            "*",
            "**",
            "`",
            "plain **unfinished",
            "a `unfinished",
            "***literal***",
            "``literal``",
        ] {
            assert_eq!(content(&markdown_lines(partial).next().unwrap()), partial);
        }
    }

    #[test]
    fn fenced_code_keeps_literal_content_and_closes_only_with_the_matching_fence() {
        let lines = markdown_lines("```rust\n**literal** `code`\n~~~\n````\nafter *em*\n")
            .collect::<Vec<_>>();
        assert_eq!(content(&lines[0]), "╭─ rust");
        assert_eq!(lines[0].style.bg, Some(CODE_BACKGROUND));
        assert_eq!(content(&lines[1]), "**literal** `code`");
        assert_eq!(lines[1].style.fg, Some(CODE_FOREGROUND));
        assert_eq!(content(&lines[2]), "~~~");
        assert_eq!(content(&lines[3]), "╰─");
        assert_eq!(content(&lines[4]), "after em");
        assert_eq!(content(&lines[5]), "");
        assert_eq!(
            content(&markdown_lines("```\nunterminated").nth(1).unwrap()),
            "unterminated"
        );
        assert_eq!(
            content(&markdown_lines("**next message**").next().unwrap()),
            "next message"
        );
        assert_eq!(
            content(&markdown_lines("~~~\ncode\n~~~").next().unwrap()),
            "╭─ code"
        );
        let language = format!("```{}", "e\u{301}".repeat(81));
        let label = content(&markdown_lines(&language).next().unwrap());
        assert_eq!(label, format!("╭─ {}…", "e\u{301}".repeat(80)));
    }

    #[test]
    fn sanitized_controls_remain_visible_without_creating_terminal_sequences() {
        let text = safe_text("## title\u{1b}]2;hostile\u{7}\n`x\u{9b}y\u{202e}z`");
        let output = markdown_lines(&text)
            .map(|line| content(&line))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(output.contains("\\u{1b}]2;hostile\\u{7}"));
        assert!(output.contains("x\\u{9b}y\\u{202e}z"));
        assert!(
            !output
                .chars()
                .any(|character| character.is_control() && character != '\n')
        );
        assert!(!output.contains('\u{202e}'));
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
        let source = "**x** plain ".repeat(10_000);
        let line = markdown_lines(&source).next().unwrap();
        assert!(line.spans.len() <= MAX_SPANS);
        assert!(content(&line).ends_with("**x** plain "));
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
        append_wrapped_line(source, usize::MAX, &mut lines, &mut count, 1);
        assert_eq!(count, 1);
        assert_eq!(content(&lines[0]).len(), 10_000);
        assert!(lines[0].spans.len() <= MAX_SPANS);
    }
}
