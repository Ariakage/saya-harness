//! Built-in, bounded code highlighting without external grammar or theme files.
use crate::theme::{CODE_BACKGROUND, CODE_FOREGROUND};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::sync::OnceLock;
use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};

const MAX_BLOCK_BYTES: usize = 32 * 1024;
const MAX_LINE_BYTES: usize = 2048;
const MAX_LINE_SPANS: usize = 256;
const MAX_BLOCK_SPANS: usize = 4096;

static RESOURCES: OnceLock<(SyntaxSet, ThemeSet)> = OnceLock::new();

/// Highlight sanitized code using bundled grammars and a fixed dark theme.
/// One trailing line ending is consumed as the code block terminator. Empty
/// input produces one blank line, and additional blank lines remain visible.
/// Unknown languages, parsing errors and budgets fall back to the whole source
/// with plain code styling, preserving every logical line's contents.
pub(super) fn highlight(source: &str, language: &str) -> Vec<Line<'static>> {
    if source.is_empty()
        || source.len() > MAX_BLOCK_BYTES
        || language.len() > 64
        || matches!(language, "" | "plain" | "text" | "txt" | "plaintext")
        || source.split('\n').any(|line| line.len() > MAX_LINE_BYTES)
    {
        return plain_lines(source);
    }
    let (syntaxes, themes) = RESOURCES.get_or_init(|| {
        (
            SyntaxSet::load_defaults_newlines(),
            ThemeSet::load_defaults(),
        )
    });
    let Some(syntax) = syntaxes.find_syntax_by_token(language) else {
        return plain_lines(source);
    };
    let Some(theme) = themes
        .themes
        .get("base16-ocean.dark")
        .or_else(|| themes.themes.values().next())
    else {
        return plain_lines(source);
    };
    let base = Style::default().fg(CODE_FOREGROUND).bg(CODE_BACKGROUND);
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut output = Vec::new();
    let mut count = 0;
    for line in LinesWithEndings::from(source) {
        let Ok(ranges) = highlighter.highlight_line(line, syntaxes) else {
            return plain_lines(source);
        };
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (style, text) in ranges {
            let text = text.strip_suffix('\n').unwrap_or(text);
            if text.is_empty() {
                continue;
            }
            let style = terminal_style(style);
            if let Some(previous) = spans.last_mut()
                && previous.style == style
            {
                previous.content.to_mut().push_str(text);
                continue;
            }
            count += 1;
            if spans.len() >= MAX_LINE_SPANS || count > MAX_BLOCK_SPANS {
                return plain_lines(source);
            }
            spans.push(Span::styled(text.to_owned(), style));
        }
        output.push(Line::from(spans).style(base));
    }
    output
}

fn terminal_style(style: syntect::highlighting::Style) -> Style {
    let mut result = Style::default()
        .fg(Color::Rgb(
            style.foreground.r,
            style.foreground.g,
            style.foreground.b,
        ))
        .bg(CODE_BACKGROUND);
    if style.font_style.contains(FontStyle::BOLD) {
        result = result.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        result = result.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        result = result.add_modifier(Modifier::UNDERLINED);
    }
    result
}

fn plain_lines(source: &str) -> Vec<Line<'static>> {
    let style = Style::default().fg(CODE_FOREGROUND).bg(CODE_BACKGROUND);
    if source.is_empty() {
        return vec![Line::from(Span::styled(String::new(), style)).style(style)];
    }
    source
        .split_terminator('\n')
        .map(|line| Line::from(Span::styled(line.to_owned(), style)).style(style))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn bundled_rust_python_javascript_and_json_have_distinct_token_colors() {
        for (language, source) in [
            ("rust", "fn main() { let value = \"hello\"; }"),
            ("python", "def greet():\n    return \"hello\""),
            ("js", "const value = \"hello\"; console.log(value);"),
            ("json", "{\"value\": true, \"count\": 42}"),
        ] {
            let lines = highlight(source, language);
            assert_eq!(text(&lines), source);
            let colors = lines
                .iter()
                .flat_map(|line| &line.spans)
                .map(|span| span.style.fg)
                .collect::<HashSet<_>>();
            assert!(colors.len() > 1, "missing syntax colors for {language}");
            assert!(
                lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .all(|span| span.style.bg == Some(CODE_BACKGROUND))
            );
        }
    }

    #[test]
    fn unknown_and_plain_languages_preserve_code_with_plain_styling() {
        let source = "**literal** <tag> 中文 e\u{301}\nnext";
        for language in ["unregistered-language", "", "text", "plain", "txt"] {
            let lines = highlight(source, language);
            assert_eq!(text(&lines), source);
            assert!(lines.iter().flat_map(|line| &line.spans).all(|span| {
                span.style.fg == Some(CODE_FOREGROUND) && span.style.bg == Some(CODE_BACKGROUND)
            }));
        }
    }

    #[test]
    fn multiline_grammars_retain_comment_state_and_unicode_source() {
        let source = "/* comment\n中文 e\u{301} 👩\u{200d}💻\n*/\nlet answer = 42;\n";
        let lines = highlight(source, "rust");
        assert_eq!(lines.len(), 4);
        assert_eq!(text(&lines), source.strip_suffix('\n').unwrap());
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|span| span.style.fg != Some(CODE_FOREGROUND))
        );
        assert!(
            lines[3]
                .spans
                .iter()
                .map(|span| span.style.fg)
                .collect::<HashSet<_>>()
                .len()
                > 1
        );
    }

    #[test]
    fn line_endings_keep_blank_lines_without_adding_a_phantom_trailing_row() {
        for (source, count, expected) in [
            ("", 1, ""),
            ("\n", 1, ""),
            ("\n\n", 2, "\n"),
            ("one\n\ntwo\n", 3, "one\n\ntwo"),
            ("one\n\ntwo", 3, "one\n\ntwo"),
        ] {
            for language in ["text", "rust"] {
                let lines = highlight(source, language);
                assert_eq!(lines.len(), count);
                assert_eq!(text(&lines), expected);
            }
        }
    }

    #[test]
    fn oversized_blocks_and_lines_use_plain_code_without_losing_content() {
        for source in [
            format!("{}\nEND", "let x = 1;\n".repeat(MAX_BLOCK_BYTES / 10)),
            format!("let text = \"{}\";\nEND", "中".repeat(MAX_LINE_BYTES)),
        ] {
            let lines = highlight(&source, "rust");
            assert_eq!(text(&lines), source);
            assert!(
                lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .all(|span| span.style.fg == Some(CODE_FOREGROUND))
            );
        }
    }

    #[test]
    fn font_modifiers_are_translated_while_using_the_shared_code_background() {
        let style = syntect::highlighting::Style {
            foreground: syntect::highlighting::Color {
                r: 1,
                g: 2,
                b: 3,
                a: 255,
            },
            background: syntect::highlighting::Color {
                r: 4,
                g: 5,
                b: 6,
                a: 255,
            },
            font_style: FontStyle::BOLD | FontStyle::ITALIC | FontStyle::UNDERLINE,
        };
        let result = terminal_style(style);
        assert_eq!(result.fg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(result.bg, Some(CODE_BACKGROUND));
        assert!(
            result
                .add_modifier
                .contains(Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED)
        );
    }
}
