//! A bounded presentation subset of Markdown, independent of runtime messages.
use crate::editor::{MAX_VISUAL_LINE_BYTES, display_grapheme};
use crate::theme::{
    AMBER, BLUE, BORDER, CODE_BACKGROUND, CODE_FOREGROUND, FOREGROUND, HEADING, INLINE_CODE, MUTED,
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

mod table;

#[derive(Clone, Copy)]
struct Fence {
    marker: u8,
    length: usize,
}

#[cfg(test)]
pub(crate) fn markdown_lines(text: &str) -> impl Iterator<Item = Line<'static>> + '_ {
    markdown_lines_with_width(text, 80)
}

/// Render sanitized assistant text with viewport-aware tables and literal fallbacks.
pub(crate) fn markdown_lines_with_width(
    text: &str,
    width: usize,
) -> impl Iterator<Item = Line<'static>> + '_ {
    let mut source = text.split('\n').peekable();
    let mut fence: Option<Fence> = None;
    let mut table: Option<table::Table> = None;
    std::iter::from_fn(move || {
        loop {
            if let Some(active) = &mut table {
                if let Some(line) = active.next(&mut source) {
                    return Some(line);
                }
                table = None;
            }
            let line = source.next()?;
            let trimmed = line.trim_start_matches(' ');
            let starts_fence = line.len() - trimmed.len() <= 3 && fence_marker(trimmed).is_some();
            if fence.is_none()
                && !starts_fence
                && let Some(separator) = source.peek()
                && let Some(next) =
                    table::Table::new(line, separator, source.clone().skip(1), width)
            {
                source.next();
                table = Some(next);
                continue;
            }
            return Some(markdown_line(line, &mut fence));
        }
    })
}

fn markdown_line(line: &str, fence: &mut Option<Fence>) -> Line<'static> {
    let trimmed = line.trim_start_matches(' ');
    let indentation = line.len() - trimmed.len();
    let candidate = (indentation <= 3).then(|| fence_marker(trimmed)).flatten();
    if let Some(open) = *fence {
        if let Some((close, rest)) = candidate
            && close.marker == open.marker
            && close.length >= open.length
            && rest.trim().is_empty()
        {
            *fence = None;
            return Line::styled("╰─", Style::default().fg(BORDER).bg(CODE_BACKGROUND));
        }
        return Line::styled(
            line.to_owned(),
            Style::default().fg(CODE_FOREGROUND).bg(CODE_BACKGROUND),
        );
    }
    if let Some((open, language)) = candidate {
        *fence = Some(open);
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
    let mut budget = text.len().saturating_mul(4);
    inline_spans_with_budget(text, base, spans, depth, &mut budget);
}

fn inline_spans_with_budget(
    text: &str,
    base: Style,
    spans: &mut Vec<Span<'static>>,
    depth: usize,
    budget: &mut usize,
) {
    if depth >= MAX_INLINE_DEPTH {
        append_span(spans, text, base);
        return;
    }
    let mut cursor = 0;
    let mut plain_start = 0;
    while cursor < text.len() && spans.len() < MAX_SPANS - 2 && *budget > 0 {
        *budget = budget.saturating_sub(1);
        let marker = text.as_bytes()[cursor];
        if marker == b'\\'
            && text
                .as_bytes()
                .get(cursor + 1)
                .is_some_and(u8::is_ascii_punctuation)
        {
            append_span(spans, &text[plain_start..cursor], base);
            append_span(spans, &text[cursor + 1..cursor + 2], base);
            cursor += 2;
            plain_start = cursor;
            continue;
        }
        if marker == b'['
            && let Some(link) = inline_link(text, cursor, budget)
        {
            append_span(spans, &text[plain_start..cursor], base);
            if let Some(destination) = link.destination
                && !text[..cursor].ends_with('!')
            {
                let style = base.fg(BLUE).add_modifier(Modifier::UNDERLINED);
                let first = spans.len();
                inline_spans_with_budget(link.label, style, spans, depth + 1, budget);
                for span in &mut spans[first..] {
                    span.style = span.style.fg(BLUE).add_modifier(Modifier::UNDERLINED);
                }
                append_span(spans, " (", base);
                append_span(spans, destination, style);
                append_span(spans, ")", base);
            } else {
                append_span(spans, &text[cursor..link.end], base);
            }
            cursor = link.end;
            plain_start = cursor;
            continue;
        }
        if marker == b'<'
            && (text[cursor + 1..].starts_with("https://")
                || text[cursor + 1..].starts_with("http://"))
            && let Some(close) = text[cursor + 1..].find('>')
            && let destination = &text[cursor + 1..cursor + 1 + close]
            && supported_url(destination)
        {
            append_span(spans, &text[plain_start..cursor], base);
            append_span(
                spans,
                destination,
                base.fg(BLUE).add_modifier(Modifier::UNDERLINED),
            );
            cursor += close + 2;
            plain_start = cursor;
            continue;
        }
        if let Some(end) = literal_token_end(text, cursor) {
            *budget = budget.saturating_sub(end - cursor);
            if matches!(marker, b'h' | b'H') && supported_url(&text[cursor..end]) {
                append_span(spans, &text[plain_start..cursor], base);
                append_span(
                    spans,
                    &text[cursor..end],
                    base.fg(BLUE).add_modifier(Modifier::UNDERLINED),
                );
                plain_start = end;
            }
            cursor = end;
            continue;
        }
        if !matches!(marker, b'*' | b'_' | b'`') {
            cursor += text[cursor..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let length = text[cursor..]
            .bytes()
            .take_while(|byte| *byte == marker)
            .count();
        if length > 2
            || (marker == b'`' && length != 1)
            || (marker == b'_'
                && (length != 2
                    || text[..cursor]
                        .chars()
                        .next_back()
                        .is_some_and(word_character)
                    || text[cursor + length..]
                        .chars()
                        .next()
                        .is_none_or(char::is_whitespace)))
        {
            cursor += length;
            continue;
        }
        let content_start = cursor + length;
        if let Some(close) = closing_marker(text, content_start, marker, length, budget)
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
                inline_spans_with_budget(
                    content,
                    base.fg(AMBER).add_modifier(modifier),
                    spans,
                    depth + 1,
                    budget,
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

fn closing_marker(
    text: &str,
    start: usize,
    marker: u8,
    length: usize,
    budget: &mut usize,
) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut cursor = start;
    while cursor < bytes.len() && *budget > 0 {
        *budget = budget.saturating_sub(1);
        if marker != b'`' && bytes[cursor] == b'\\' && cursor + 1 < bytes.len() {
            cursor += 2;
            continue;
        }
        if marker != b'`'
            && bytes[cursor] == b'['
            && let Some(link) = inline_link(text, cursor, budget)
        {
            cursor = link.end;
            continue;
        }
        if marker != b'`'
            && let Some(end) = literal_token_end(text, cursor)
        {
            *budget = budget.saturating_sub(end - cursor);
            if text[cursor..end].starts_with("https://") || text[cursor..end].starts_with("http://")
            {
                let trailing = bytes[cursor..end]
                    .iter()
                    .rev()
                    .take_while(|byte| **byte == marker)
                    .count();
                let close = end - trailing;
                // Only a distinct trailing delimiter closes emphasis around a URL.
                // Markers within the URL continue to be literal URL content.
                if trailing == length
                    && !bytes[cursor..close]
                        .windows(length)
                        .any(|run| run.iter().all(|byte| *byte == marker))
                    && supported_url(&text[cursor..close])
                    && (marker != b'_' || !text[end..].chars().next().is_some_and(word_character))
                {
                    return Some(close);
                }
            }
            cursor = end;
            continue;
        }
        if marker != b'`' && bytes[cursor] == b'`' {
            let run = bytes[cursor..]
                .iter()
                .take_while(|byte| **byte == b'`')
                .count();
            if run == 1
                && let Some(close) = closing_marker(text, cursor + 1, b'`', 1, budget)
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
        if run == length
            && (marker != b'_'
                || (!text[..cursor]
                    .chars()
                    .next_back()
                    .is_none_or(char::is_whitespace)
                    && !text[cursor + run..]
                        .chars()
                        .next()
                        .is_some_and(word_character)))
        {
            return Some(cursor);
        }
        cursor += run;
    }
    None
}

fn word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

struct InlineLink<'a> {
    label: &'a str,
    destination: Option<&'a str>,
    end: usize,
}

fn inline_link<'a>(text: &'a str, start: usize, budget: &mut usize) -> Option<InlineLink<'a>> {
    let bytes = text.as_bytes();
    let mut cursor = start + 1;
    let mut brackets = 1usize;
    while cursor < bytes.len() && *budget > 0 {
        *budget = budget.saturating_sub(1);
        match bytes[cursor] {
            b'\\' if cursor + 1 < bytes.len() => cursor += 1,
            b'`' => {
                let run = bytes[cursor..]
                    .iter()
                    .take_while(|byte| **byte == b'`')
                    .count();
                if run == 1 {
                    let Some(close) = closing_marker(text, cursor + 1, b'`', 1, budget) else {
                        return Some(InlineLink {
                            label: "",
                            destination: None,
                            end: text.len(),
                        });
                    };
                    cursor = close;
                } else {
                    cursor += run - 1;
                }
            }
            b'[' => brackets += 1,
            b']' => {
                brackets -= 1;
                if brackets == 0 {
                    break;
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    let label_end = cursor;
    if brackets > 0 {
        return Some(InlineLink {
            label: "",
            destination: None,
            end: text.len(),
        });
    }
    if bytes.get(cursor + 1) != Some(&b'(') {
        return None;
    }
    let destination_start = cursor + 2;
    cursor = destination_start;
    let mut parentheses = 1usize;
    while cursor < bytes.len() && *budget > 0 {
        *budget = budget.saturating_sub(1);
        match bytes[cursor] {
            b'\\' if cursor + 1 < bytes.len() => cursor += 1,
            b'(' => parentheses += 1,
            b')' => {
                parentheses -= 1;
                if parentheses == 0 {
                    break;
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    if parentheses > 0 {
        return Some(InlineLink {
            label: "",
            destination: None,
            end: text.len(),
        });
    }
    let destination = text[destination_start..cursor].trim();
    let destination = destination
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .unwrap_or(destination);
    Some(InlineLink {
        label: &text[start + 1..label_end],
        destination: supported_url(destination).then_some(destination),
        end: cursor + 1,
    })
}

fn supported_url(text: &str) -> bool {
    let authority = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"));
    authority.is_some_and(|rest| !rest.is_empty())
        && !text.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || matches!(character, '<' | '>' | '"')
        })
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
/// Oversized clusters become an ellipsis, and byte-limited rows show continuation.
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
    fn markdown_links_show_the_destination_and_keep_label_formatting() {
        let line =
            markdown_lines("Read [the **guide** and `API`](https://example.test/docs_(v2)) now.")
                .next()
                .unwrap();
        assert_eq!(
            content(&line),
            "Read the guide and API (https://example.test/docs_(v2)) now."
        );
        let bold = line
            .spans
            .iter()
            .find(|span| span.content == "guide")
            .unwrap();
        assert_eq!(bold.style.fg, Some(BLUE));
        assert!(
            bold.style
                .add_modifier
                .contains(Modifier::BOLD | Modifier::UNDERLINED)
        );
        let code = line
            .spans
            .iter()
            .find(|span| span.content == "API")
            .unwrap();
        assert_eq!(code.style.bg, Some(CODE_BACKGROUND));
        assert_eq!(code.style.fg, Some(BLUE));
        let target = line
            .spans
            .iter()
            .find(|span| span.content == "https://example.test/docs_(v2)")
            .unwrap();
        assert!(target.style.add_modifier.contains(Modifier::UNDERLINED));
        let nested = markdown_lines("**[bold link](https://example.test)**")
            .next()
            .unwrap();
        assert_eq!(content(&nested), "bold link (https://example.test)");
        let label = nested
            .spans
            .iter()
            .find(|span| span.content == "bold link")
            .unwrap();
        assert!(
            label
                .style
                .add_modifier
                .contains(Modifier::BOLD | Modifier::UNDERLINED)
        );
    }

    #[test]
    fn autolinks_and_bare_urls_are_underlined_without_parsing_url_markers() {
        let line = markdown_lines("<https://example.test/a_b> https://example.test/**x**")
            .next()
            .unwrap();
        assert_eq!(
            content(&line),
            "https://example.test/a_b https://example.test/**x**"
        );
        for span in line
            .spans
            .iter()
            .filter(|span| span.content.starts_with("https://"))
        {
            assert_eq!(span.style.fg, Some(BLUE));
            assert!(span.style.add_modifier.contains(Modifier::UNDERLINED));
        }
    }

    #[test]
    fn code_brackets_in_link_labels_are_literal_and_do_not_close_the_label() {
        for (source, expected) in [
            (
                "[use `[`](https://example.test)",
                "use [ (https://example.test)",
            ),
            ("[`]`](https://example.test)", "] (https://example.test)"),
            (
                "[use `[x]` and **care**](https://example.test)",
                "use [x] and care (https://example.test)",
            ),
        ] {
            let line = markdown_lines(source).next().unwrap();
            assert_eq!(content(&line), expected);
            assert!(
                line.spans
                    .iter()
                    .any(|span| span.style.bg == Some(CODE_BACKGROUND))
            );
        }
        let partial = "[use `unfinished [label](https://example.test)";
        assert_eq!(content(&markdown_lines(partial).next().unwrap()), partial);
    }

    #[test]
    fn emphasis_can_wrap_a_url_with_a_distinct_trailing_delimiter() {
        for source in [
            "**https://example.test**",
            "__https://example.test__",
            "**see https://example.test**",
        ] {
            let line = markdown_lines(source).next().unwrap();
            assert!(!content(&line).contains("**"));
            assert!(!content(&line).contains("__"));
            let url = line
                .spans
                .iter()
                .find(|span| span.content == "https://example.test")
                .unwrap();
            assert_eq!(url.style.fg, Some(BLUE));
            assert!(
                url.style
                    .add_modifier
                    .contains(Modifier::BOLD | Modifier::UNDERLINED)
            );
        }
        for (source, expected) in [
            (
                "**https://example.test/a*b*c**",
                "https://example.test/a*b*c",
            ),
            ("__https://example.test/a_b__", "https://example.test/a_b"),
        ] {
            let line = markdown_lines(source).next().unwrap();
            assert_eq!(content(&line), expected);
            assert!(
                line.spans[0]
                    .style
                    .add_modifier
                    .contains(Modifier::BOLD | Modifier::UNDERLINED)
            );
        }
        for source in [
            "https://example.test/**x**",
            "https://example.test/__x__",
            "https://example.test/a*b*c",
        ] {
            assert_eq!(content(&markdown_lines(source).next().unwrap()), source);
        }
    }

    #[test]
    fn incomplete_unsupported_and_image_links_remain_literal() {
        for source in [
            "[**unfinished",
            "[**label**](https://example.test/unfinished",
            "[label](javascript:alert(1))",
            "[label](file:///tmp/example)",
            "[label](https://)",
            "[label](https://example.test \"title\")",
            "![image](https://example.test/image.png)",
        ] {
            assert_eq!(content(&markdown_lines(source).next().unwrap()), source);
        }
        let code = markdown_lines("`[literal](https://example.test)`")
            .next()
            .unwrap();
        assert_eq!(content(&code), "[literal](https://example.test)");
        assert_eq!(code.spans[0].style.fg, Some(INLINE_CODE));
    }

    #[test]
    fn underscore_bold_respects_word_boundaries_and_unicode() {
        let line = markdown_lines("__加粗__ and __strong__").next().unwrap();
        assert_eq!(content(&line), "加粗 and strong");
        for span in line.spans.iter().filter(|span| span.content != " and ") {
            assert!(span.style.add_modifier.contains(Modifier::BOLD));
        }
        for source in [
            "foo__bar__",
            "__bold__suffix",
            "__ unfinished__",
            "___literal___",
        ] {
            assert_eq!(content(&markdown_lines(source).next().unwrap()), source);
        }
    }

    #[test]
    fn escaped_markdown_delimiters_and_unicode_link_labels_are_preserved() {
        let source = r"\[plain](https://example.test) \*\*literal\*\* \_value\_ [中\]文](https://example.test/路径_(一))";
        let line = markdown_lines(source).next().unwrap();
        assert_eq!(
            content(&line),
            "[plain](https://example.test) **literal** _value_ 中]文 (https://example.test/路径_(一))"
        );
        for source in ["\\中文", "[中](https://example.test/\\路径)", "__中\\文__"] {
            let _ = markdown_lines(source).collect::<Vec<_>>();
        }
    }

    #[test]
    fn link_display_does_not_reintroduce_sanitized_terminal_controls() {
        let source = safe_text("[safe\u{1b}]2;title\u{7}](https://example.test/\u{202e}path)");
        let output = content(&markdown_lines(&source).next().unwrap());
        assert!(output.contains("\\u{1b}]2;title\\u{7}"));
        assert!(output.contains("\\u{202e}path"));
        assert!(!output.chars().any(char::is_control));
        assert!(!output.contains('\u{202e}'));
    }

    #[test]
    fn adversarial_link_lookahead_uses_a_shared_scan_budget() {
        for source in [
            "[".repeat(64 * 1024),
            "[[x] ".repeat(10_000),
            "<".repeat(64 * 1024),
        ] {
            let mut spans = Vec::new();
            let mut budget = source.len().saturating_mul(4);
            inline_spans_with_budget(&source, Style::default(), &mut spans, 0, &mut budget);
            assert_eq!(content(&Line::from(spans.clone())), source);
            assert!(spans.len() <= MAX_SPANS);
        }
        let source = "[label](https://example.test) **rest**";
        let mut spans = Vec::new();
        let mut budget = 3;
        inline_spans_with_budget(source, Style::default(), &mut spans, 0, &mut budget);
        assert_eq!(budget, 0);
        assert_eq!(content(&Line::from(spans)), source);
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
