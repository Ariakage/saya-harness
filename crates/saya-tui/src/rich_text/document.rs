//! CommonMark/GFM events projected into bounded terminal lines and semantic anchors.
use super::{append_span, append_wrapped_line_observed, math, mermaid, syntax, table};
use crate::theme::{
    AMBER, BLUE, BORDER, CODE_BACKGROUND, CODE_FOREGROUND, FOREGROUND, GREEN, HEADING, INLINE_CODE,
    MUTED,
};
use crate::{editor::safe_text, i18n::rich_text as ui_text};
use pulldown_cmark::{CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ops::Range,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MAX_SOURCE_BYTES: usize = 256 * 1024;
const MAX_EVENTS: usize = 65_536;
const MAX_DEPTH: usize = 64;
const MAX_LINES: usize = 10_000;
const MAX_ANCHORS: usize = 4096;

#[derive(Clone, Default)]
pub(super) struct RichLine {
    pub line: Line<'static>,
    pub anchors: Vec<ByteAnchor>,
}

#[derive(Clone)]
pub(super) struct ByteAnchor {
    pub key: String,
    pub number: usize,
    pub offset: usize,
    pub length: usize,
    pub definition: bool,
    pub occurrence: usize,
}

pub(crate) struct FootnoteAnchor {
    pub key: String,
    pub number: usize,
    pub line: usize,
    pub column: usize,
    pub width: usize,
    pub definition: bool,
    pub occurrence: usize,
}

pub(crate) struct RenderedDocument {
    pub lines: Vec<Line<'static>>,
    pub footnotes: Vec<FootnoteAnchor>,
}

pub(crate) fn render_document(text: &str, width: usize) -> RenderedDocument {
    if text.len() > MAX_SOURCE_BYTES {
        return source_fallback(text, width, ui_text::MARKDOWN_SOURCE_BUDGET);
    }
    let normalized = normalize(text);
    if normalized.incomplete_math {
        return source_fallback(text, width, ui_text::MATH_SOURCE_FALLBACK);
    }
    if normalized.structure_exceeded {
        return source_fallback(text, width, ui_text::MARKDOWN_STRUCTURE_BUDGET);
    }
    let events = Parser::new_ext(&normalized.text, options())
        .into_offset_iter()
        .take(MAX_EVENTS + 1)
        .collect::<Vec<_>>();
    let mut depth = 0usize;
    if events.len() > MAX_EVENTS
        || events.iter().any(|(event, _)| {
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) => depth = depth.saturating_sub(1),
                _ => {}
            }
            depth > MAX_DEPTH
        })
    {
        return source_fallback(text, width, ui_text::MARKDOWN_STRUCTURE_BUDGET);
    }
    let definitions = events
        .iter()
        .filter_map(|(event, _)| match event {
            Event::Start(Tag::FootnoteDefinition(key)) => Some(canonical_key(key)),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let mut numbers = BTreeMap::new();
    // Definitions may precede references. Number by first reference order.
    for references in [true, false] {
        for (event, _) in &events {
            let key = match (references, event) {
                (true, Event::FootnoteReference(key))
                | (false, Event::Start(Tag::FootnoteDefinition(key))) => canonical_key(key),
                _ => continue,
            };
            if definitions.contains(&key) && key.len() <= 256 && numbers.len() < 512 {
                let next = numbers.len() + 1;
                numbers.entry(key).or_insert(next);
            }
        }
    }
    let mut renderer = Renderer::new(width, &normalized, numbers);
    for (event, range) in events {
        renderer.event(event, range);
    }
    renderer.finish(text.ends_with('\n'))
}

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM
}

fn canonical_key(key: &str) -> String {
    unicase::UniCase::unicode(key.split_whitespace().collect::<Vec<_>>().join(" ")).to_folded_case()
}

struct Normalized {
    text: String,
    math: BTreeMap<usize, String>,
    incomplete_math: bool,
    structure_exceeded: bool,
}

/// Normalize view extensions while preserving code, HTML and URI destinations.
fn normalize(text: &str) -> Normalized {
    let mut protected = vec![false; text.len()];
    let mut verbatim = vec![false; text.len()];
    let mut label_text = Vec::new();
    let mut links = Vec::new();
    let mut emphasis = Vec::new();
    let mut remaining = text.len().saturating_mul(8);
    let mut depth = 0usize;
    let parser = Parser::new_ext(text, options());
    for (_, definition) in parser.reference_definitions().iter() {
        if let Some(slice) = protected.get_mut(definition.span.clone()) {
            slice.fill(true);
        }
    }
    for (event, range) in parser.into_offset_iter().take(MAX_EVENTS) {
        match &event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth > MAX_DEPTH {
            return Normalized {
                text: text.to_owned(),
                math: BTreeMap::new(),
                incomplete_math: false,
                structure_exceeded: true,
            };
        }
        match &event {
            Event::Start(Tag::Link { link_type, .. }) => {
                links.push(!matches!(link_type, LinkType::Autolink | LinkType::Email))
            }
            Event::Start(Tag::Image { .. }) => links.push(true),
            Event::End(TagEnd::Link | TagEnd::Image) => {
                links.pop();
            }
            Event::Text(_) if links.last() == Some(&true) => {
                // Synthesized escaped punctuation ranges omit the source backslash.
                let start = if range.start > 0
                    && text.as_bytes().get(range.start - 1) == Some(&b'\\')
                    && text
                        .as_bytes()
                        .get(range.start)
                        .is_some_and(u8::is_ascii_punctuation)
                {
                    range.start - 1
                } else {
                    range.start
                };
                label_text.push(start..range.end);
            }
            _ => {}
        }
        let literal = matches!(
            &event,
            Event::Code(_)
                | Event::Html(_)
                | Event::InlineHtml(_)
                | Event::InlineMath(_)
                | Event::DisplayMath(_)
                | Event::Start(Tag::CodeBlock(_) | Tag::HtmlBlock)
                | Event::Start(Tag::Link {
                    link_type: LinkType::Autolink | LinkType::Email,
                    ..
                })
        );
        match event {
            _ if literal
                || matches!(&event, Event::Start(Tag::Link { .. } | Tag::Image { .. })) =>
            {
                if let Some(slice) = protected.get_mut(range.clone()) {
                    if slice.len() > remaining {
                        return Normalized {
                            text: text.to_owned(),
                            math: BTreeMap::new(),
                            incomplete_math: false,
                            structure_exceeded: true,
                        };
                    }
                    remaining -= slice.len();
                    slice.fill(true);
                }
                if literal {
                    // CommonMark already leaves entities in these contexts literal.
                    // Re-escaping them would change code, HTML and automatic-link URLs.
                    if let Some(slice) = verbatim.get_mut(range) {
                        slice.fill(true);
                    }
                }
            }
            Event::Start(Tag::Strong | Tag::Emphasis) => emphasis.push(range),
            _ => {}
        }
    }
    let mut math_protected = protected.clone();
    for range in label_text {
        if let Some(slice) = math_protected.get_mut(range) {
            slice.fill(false);
        }
    }
    emphasis.sort_by_key(|range| range.start);
    let bytes = text.as_bytes();
    let mut result = Normalized {
        text: String::with_capacity(text.len()),
        math: BTreeMap::new(),
        incomplete_math: false,
        structure_exceeded: false,
    };
    let mut cursor = 0;
    let mut emphasis_cursor = 0;
    let mut active: Vec<Range<usize>> = Vec::new();
    while cursor < text.len() {
        if !math_protected[cursor] && bytes[cursor] == b'\\' && cursor + 1 < text.len() {
            if bytes[cursor + 1] == b'\\' {
                result.text.push_str("\\\\");
                cursor += 2;
                continue;
            }
            if matches!(bytes[cursor + 1], b'(' | b'[') {
                let close = if bytes[cursor + 1] == b'(' {
                    b')'
                } else {
                    b']'
                };
                let mut end = cursor + 2;
                while end + 1 < text.len() {
                    if !math_protected[end] && bytes[end] == b'\\' && bytes[end + 1] == close {
                        break;
                    }
                    end += 1;
                }
                if end + 1 < text.len() {
                    let delimiter = if close == b')' { "$" } else { "$$" };
                    result
                        .math
                        .insert(result.text.len(), text[cursor..end + 2].to_owned());
                    result.text.push_str(delimiter);
                    result.text.push_str(&text[cursor + 2..end]);
                    result.text.push_str(delimiter);
                    cursor = end + 2;
                    continue;
                }
                // One unsuccessful search consumes the rest, keeping this pass linear.
                result.incomplete_math = true;
                break;
            }
        }
        if !protected[cursor]
            && (text[cursor..].starts_with("https://") || text[cursor..].starts_with("http://"))
        {
            let mut end = cursor;
            for (offset, character) in text[cursor..].char_indices() {
                if character.is_whitespace() || matches!(character, '<' | '>') {
                    break;
                }
                end = cursor + offset + character.len_utf8();
            }
            while emphasis_cursor < emphasis.len() && emphasis[emphasis_cursor].start < cursor {
                if emphasis[emphasis_cursor].end > cursor {
                    active.push(emphasis[emphasis_cursor].clone());
                }
                emphasis_cursor += 1;
            }
            active.retain(|range| range.end > cursor);
            for range in &active {
                let trailing = text[range.start..range.end]
                    .bytes()
                    .rev()
                    .take_while(|b| matches!(b, b'*' | b'_'))
                    .count();
                end = end.min(range.end.saturating_sub(trailing));
            }
            while end > cursor && matches!(bytes[end - 1], b'.' | b',' | b';' | b':' | b'!' | b'?')
            {
                end -= 1;
            }
            let balance = text[cursor..end]
                .bytes()
                .fold(0isize, |n, byte| match byte {
                    b'(' => n + 1,
                    b')' => n - 1,
                    _ => n,
                });
            let mut unmatched = balance.min(0).unsigned_abs();
            while unmatched > 0 && end > cursor && bytes[end - 1] == b')' {
                end -= 1;
                unmatched -= 1;
            }
            if end > cursor {
                result.text.push('<');
                result.text.push_str(&text[cursor..end]);
                result.text.push('>');
                cursor = end;
                continue;
            }
        }
        let Some(character) = text[cursor..].chars().next() else {
            break;
        };
        if character == '&' && !verbatim[cursor] {
            // Decoding a reference could reconstruct credentials or controls after
            // the upstream sanitizer. Keep its source spelling in rendered prose.
            result.text.push_str("&amp;");
        } else {
            result.text.push(character);
        }
        cursor += character.len_utf8();
    }
    result
}

enum Container {
    Quote,
    Item {
        marker: String,
        pending: bool,
    },
    Note {
        key: String,
        number: usize,
        pending: bool,
    },
}
struct List {
    next: Option<u64>,
}
struct CodeBlock {
    language: String,
    source: String,
    raw_range: Range<usize>,
    fenced: bool,
}
#[derive(Default)]
struct TableBlock {
    alignments: Vec<pulldown_cmark::Alignment>,
    header: Vec<RichLine>,
    rows: Vec<Vec<RichLine>>,
    row: Vec<RichLine>,
    raw_range: Range<usize>,
}

struct Renderer<'a> {
    input: &'a Normalized,
    width: usize,
    lines: VecDeque<RichLine>,
    omitted: bool,
    current: RichLine,
    style: Style,
    styles: Vec<Style>,
    containers: Vec<Container>,
    lists: Vec<List>,
    links: Vec<(String, usize, bool)>,
    code: Option<CodeBlock>,
    table: Option<TableBlock>,
    last_block_end: usize,
    numbers: BTreeMap<String, usize>,
    occurrences: BTreeMap<String, usize>,
}
impl<'a> Renderer<'a> {
    fn new(width: usize, input: &'a Normalized, numbers: BTreeMap<String, usize>) -> Self {
        Self {
            input,
            width: width.max(1),
            lines: VecDeque::new(),
            omitted: false,
            current: RichLine::default(),
            style: base_style(),
            styles: Vec::new(),
            containers: Vec::new(),
            lists: Vec::new(),
            links: Vec::new(),
            code: None,
            table: None,
            last_block_end: 0,
            numbers,
            occurrences: BTreeMap::new(),
        }
    }
    fn event(&mut self, event: Event<'_>, range: Range<usize>) {
        let ends_block = matches!(&event, Event::End(_) | Event::Rule);
        if let Some(code) = &mut self.code {
            match event {
                Event::Text(text) => code.source.push_str(&text),
                Event::End(TagEnd::CodeBlock) => self.end_code(),
                _ => {}
            }
            self.last_block_end = self.last_block_end.max(range.end);
            return;
        }
        match event {
            Event::Start(Tag::Paragraph) => self.prepare(range.start),
            Event::End(TagEnd::Paragraph) => self.flush(false),
            Event::Start(Tag::Heading { .. }) => {
                self.prepare(range.start);
                self.push_style(self.style.fg(HEADING).add_modifier(Modifier::BOLD));
            }
            Event::End(TagEnd::Heading(_)) => {
                self.flush(false);
                self.pop_style();
            }
            Event::Start(Tag::BlockQuote(_)) => {
                self.flush(false);
                self.push_style(self.style.fg(MUTED));
                self.containers.push(Container::Quote);
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                self.flush(false);
                self.containers.pop();
                self.pop_style();
            }
            Event::Start(Tag::List(next)) => {
                self.flush(self.pending_marker());
                self.lists.push(List { next });
            }
            Event::End(TagEnd::List(_)) => {
                self.flush(false);
                self.lists.pop();
            }
            Event::Start(Tag::Item) => {
                self.prepare(range.start);
                let marker = match self.lists.last_mut().and_then(|list| list.next.as_mut()) {
                    Some(next) => {
                        let marker = format!("{next}. ");
                        *next = next.saturating_add(1);
                        marker
                    }
                    None => "- ".into(),
                };
                self.containers.push(Container::Item {
                    marker,
                    pending: true,
                });
            }
            Event::End(TagEnd::Item) => {
                self.flush(self.pending_marker());
                self.containers.pop();
            }
            Event::Start(Tag::FootnoteDefinition(key)) => {
                self.prepare(range.start);
                let key = canonical_key(&key);
                if let Some(number) = self.numbers.get(&key).copied() {
                    self.containers.push(Container::Note {
                        key,
                        number,
                        pending: true,
                    });
                } else {
                    self.containers.push(Container::Item {
                        marker: format!("[^{key}] "),
                        pending: true,
                    });
                }
            }
            Event::End(TagEnd::FootnoteDefinition) => {
                self.flush(self.pending_marker());
                self.containers.pop();
            }
            Event::FootnoteReference(key) => self.reference(&key),
            Event::Start(Tag::Strong) => {
                self.push_style(self.text_style(AMBER).add_modifier(Modifier::BOLD))
            }
            Event::Start(Tag::Emphasis) => {
                self.push_style(self.text_style(AMBER).add_modifier(Modifier::ITALIC))
            }
            Event::Start(Tag::Strikethrough) => {
                self.push_style(self.style.add_modifier(Modifier::CROSSED_OUT))
            }
            Event::End(TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough) => {
                self.pop_style()
            }
            Event::Start(Tag::Link { dest_url, .. }) => self.start_link(&dest_url, false),
            Event::Start(Tag::Image { dest_url, .. }) => self.start_link(&dest_url, true),
            Event::End(TagEnd::Link | TagEnd::Image) => self.end_link(),
            Event::Code(text) => {
                self.append(&text, self.text_style(INLINE_CODE).bg(CODE_BACKGROUND))
            }
            Event::Text(text) => self.append(&text, self.style),
            Event::InlineHtml(text) => self.append(&text, self.style.fg(CODE_FOREGROUND)),
            Event::Html(text) => {
                self.append_multiline(text.trim_end_matches('\n'), self.style.fg(CODE_FOREGROUND))
            }
            Event::Start(Tag::HtmlBlock) => self.prepare(range.start),
            Event::End(TagEnd::HtmlBlock) => self.flush(false),
            Event::SoftBreak | Event::HardBreak => self.flush(true),
            Event::TaskListMarker(checked) => self.append(
                if checked { "[x] " } else { "[ ] " },
                self.style.fg(if checked { GREEN } else { MUTED }),
            ),
            Event::Rule => {
                self.prepare(range.start);
                self.emit(RichLine::plain(
                    "─".repeat(self.content_width().min(80)),
                    Style::default().fg(BORDER),
                ));
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                self.prepare(range.start);
                let (language, fenced) = match kind {
                    CodeBlockKind::Fenced(_) => (
                        fenced_language(self.input.text.get(range.clone()).unwrap_or("")),
                        true,
                    ),
                    CodeBlockKind::Indented => (ui_text::CODE_LABEL.into(), false),
                };
                self.code = Some(CodeBlock {
                    language,
                    source: String::new(),
                    raw_range: range.clone(),
                    fenced,
                });
            }
            Event::InlineMath(text) => {
                if let Some(result) = math::render(&text) {
                    self.append(&result, self.text_style(INLINE_CODE));
                } else {
                    let raw = self
                        .input
                        .math
                        .get(&range.start)
                        .map(String::as_str)
                        .unwrap_or(&self.input.text[range.clone()]);
                    self.append(raw, self.style.fg(CODE_FOREGROUND).bg(CODE_BACKGROUND));
                }
            }
            Event::DisplayMath(text) => {
                self.prepare(range.start);
                if let Some(result) = math::render_block(&text)
                    .filter(|result| math_fits(result, self.content_width()))
                {
                    self.math_card(&result);
                } else {
                    let raw = self
                        .input
                        .math
                        .get(&range.start)
                        .cloned()
                        .unwrap_or_else(|| self.input.text[range.clone()].to_owned());
                    self.code_card(&raw, ui_text::MATH_LABEL, true);
                }
            }
            Event::Start(Tag::Table(alignments)) => {
                self.prepare(range.start);
                self.table = Some(TableBlock {
                    alignments,
                    raw_range: range.clone(),
                    ..Default::default()
                });
            }
            Event::Start(Tag::TableHead | Tag::TableRow) => {}
            Event::Start(Tag::TableCell) => {
                self.current = RichLine::default();
                let header = self
                    .table
                    .as_ref()
                    .is_some_and(|table| table.header.is_empty());
                self.push_style(if header {
                    self.style.add_modifier(Modifier::BOLD)
                } else {
                    self.style
                });
            }
            Event::End(TagEnd::TableCell) => {
                self.pop_style();
                if let Some(table) = &mut self.table {
                    table.row.push(std::mem::take(&mut self.current));
                }
            }
            Event::End(TagEnd::TableHead) => {
                if let Some(table) = &mut self.table {
                    table.header = std::mem::take(&mut table.row);
                }
            }
            Event::End(TagEnd::TableRow) => {
                if let Some(table) = &mut self.table {
                    table.rows.push(std::mem::take(&mut table.row));
                }
            }
            Event::End(TagEnd::Table) => self.end_table(),
            _ => {}
        }
        if ends_block {
            self.last_block_end = self.last_block_end.max(range.end);
        }
    }
    fn prepare(&mut self, start: usize) {
        if self.table.is_some() {
            return;
        }
        self.flush(false);
        if start >= self.last_block_end
            && self.input.text[self.last_block_end..start].contains('\n')
            && !self
                .lines
                .back()
                .is_some_and(|line| line.line.spans.is_empty())
        {
            self.emit(RichLine::default());
        }
    }
    fn append(&mut self, text: &str, style: Style) {
        append_span(&mut self.current.line.spans, &safe_text(text), style);
    }
    fn text_style(&self, color: Color) -> Style {
        self.style
            .fg(if self.links.is_empty() { color } else { BLUE })
    }
    fn append_multiline(&mut self, text: &str, style: Style) {
        for (index, line) in text.split('\n').enumerate() {
            if index > 0 {
                self.flush(true);
            }
            self.append(line, style);
        }
    }
    fn push_style(&mut self, style: Style) {
        self.styles.push(self.style);
        self.style = style;
    }
    fn pop_style(&mut self) {
        self.style = self.styles.pop().unwrap_or_else(base_style);
    }
    fn start_link(&mut self, destination: &str, image: bool) {
        if image {
            self.append(ui_text::IMAGE_LABEL, self.style.fg(MUTED));
        }
        self.links
            .push((safe_text(destination), self.current.byte_len(), image));
        self.push_style(self.style.fg(BLUE).add_modifier(Modifier::UNDERLINED));
    }
    fn end_link(&mut self) {
        let Some((destination, start, image)) = self.links.pop() else {
            return;
        };
        let plain = self.current.text();
        let label = plain.get(start..).unwrap_or("");
        if image
            || (label != destination
                && label != destination.strip_prefix("mailto:").unwrap_or(&destination))
        {
            self.append(" (", base_style());
            self.append(
                &destination,
                self.style.fg(BLUE).add_modifier(Modifier::UNDERLINED),
            );
            self.append(")", base_style());
        }
        self.pop_style();
    }
    fn reference(&mut self, key: &str) {
        let key = canonical_key(key);
        let Some(number) = self.numbers.get(&key).copied() else {
            self.append(&format!("[^{key}]"), self.style);
            return;
        };
        let occurrence = self.occurrences.entry(key.clone()).or_default();
        let current = *occurrence;
        *occurrence += 1;
        let marker = format!("[{number}]");
        if self.current.anchors.len() < MAX_ANCHORS {
            self.current.anchors.push(ByteAnchor {
                key,
                number,
                offset: self.current.byte_len(),
                length: marker.len(),
                definition: false,
                occurrence: current,
            });
        }
        self.append(&marker, footnote_style(self.style));
    }
    fn content_width(&self) -> usize {
        self.width.saturating_sub(self.prefix_width()).max(1)
    }
    fn prefix_width(&self) -> usize {
        self.containers
            .iter()
            .map(|container| match container {
                Container::Quote => 2,
                Container::Item { marker, .. } => marker.width(),
                Container::Note { number, .. } => number.to_string().len() + 3,
            })
            .sum()
    }
    fn pending_marker(&self) -> bool {
        matches!(
            self.containers.last(),
            Some(Container::Item { pending: true, .. } | Container::Note { pending: true, .. })
        )
    }
    fn prefix(&mut self) -> RichLine {
        let mut prefix = RichLine::default();
        for container in &mut self.containers {
            match container {
                Container::Quote => {
                    append_span(&mut prefix.line.spans, "│ ", Style::default().fg(MUTED))
                }
                Container::Item { marker, pending } => {
                    append_span(
                        &mut prefix.line.spans,
                        &if *pending {
                            marker.clone()
                        } else {
                            " ".repeat(marker.width())
                        },
                        Style::default().fg(AMBER),
                    );
                    *pending = false;
                }
                Container::Note {
                    key,
                    number,
                    pending,
                } => {
                    let marker = format!("[{number}] ");
                    if *pending {
                        prefix.anchors.push(ByteAnchor {
                            key: key.clone(),
                            number: *number,
                            offset: prefix.byte_len(),
                            length: marker.len() - 1,
                            definition: true,
                            occurrence: 0,
                        });
                        append_span(
                            &mut prefix.line.spans,
                            &marker,
                            footnote_style(base_style()),
                        );
                    } else {
                        append_span(
                            &mut prefix.line.spans,
                            &" ".repeat(marker.len()),
                            base_style(),
                        );
                    }
                    *pending = false;
                }
            }
        }
        prefix
    }
    fn flush(&mut self, force: bool) {
        if self.table.is_some() || (self.current.line.spans.is_empty() && !force) {
            return;
        }
        let line = std::mem::take(&mut self.current);
        self.emit(line);
    }
    fn emit(&mut self, line: RichLine) {
        let mut prefixed = self.prefix();
        prefixed.append(line);
        for physical in wrap_line(prefixed, self.width, MAX_LINES + 1) {
            self.push(physical);
        }
    }
    fn push(&mut self, line: RichLine) {
        if self.lines.len() == MAX_LINES {
            self.lines.pop_front();
            self.omitted = true;
        }
        self.lines.push_back(line);
    }
    fn end_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        match table::render(
            table.header,
            table.rows,
            &table.alignments,
            self.content_width(),
        ) {
            Some(lines) => {
                for line in lines {
                    self.emit(line);
                }
            }
            None => {
                let raw = self
                    .input
                    .text
                    .get(table.raw_range)
                    .unwrap_or("")
                    .to_owned();
                self.code_card(&raw, ui_text::TABLE_LABEL, true);
            }
        }
    }
    fn end_code(&mut self) {
        let Some(code) = self.code.take() else { return };
        let language = code.language.to_ascii_lowercase();
        let raw = self.input.text.get(code.raw_range).unwrap_or("");
        let complete = !code.fenced || closed_fence(raw);
        let source = code.source.strip_suffix('\n').unwrap_or(&code.source);
        if complete && matches!(language.as_str(), "math" | "latex" | "tex") {
            if let Some(rendered) =
                math::render_block(source).filter(|result| math_fits(result, self.content_width()))
            {
                self.math_card(&rendered);
                return;
            }
        } else if complete
            && language == "mermaid"
            && let Some(lines) = mermaid::render(source, self.content_width())
        {
            self.card_header(ui_text::MERMAID_LABEL);
            for line in lines {
                self.emit(RichLine::plain(
                    line,
                    Style::default().fg(CODE_FOREGROUND).bg(CODE_BACKGROUND),
                ));
            }
            self.emit(RichLine::plain(
                "╰─",
                Style::default().fg(BORDER).bg(CODE_BACKGROUND),
            ));
            return;
        }
        self.code_card(
            &code.source,
            &code.language,
            matches!(language.as_str(), "math" | "latex" | "tex" | "mermaid"),
        );
    }
    fn code_card(&mut self, source: &str, language: &str, fallback: bool) {
        let label = language.chars().take(80).collect::<String>();
        self.card_header(&format!(
            "{label}{}",
            if fallback {
                ui_text::SOURCE_FALLBACK_SUFFIX
            } else {
                ""
            }
        ));
        for line in syntax::highlight(source, language) {
            self.emit(RichLine {
                line,
                anchors: Vec::new(),
            });
        }
        self.emit(RichLine::plain(
            "╰─",
            Style::default().fg(BORDER).bg(CODE_BACKGROUND),
        ));
    }
    fn math_card(&mut self, rendered: &str) {
        self.card_header(ui_text::MATH_LABEL);
        for line in rendered.split('\n') {
            self.emit(RichLine::plain(
                line.to_owned(),
                Style::default().fg(INLINE_CODE).bg(CODE_BACKGROUND),
            ));
        }
        self.emit(RichLine::plain(
            "╰─",
            Style::default().fg(BORDER).bg(CODE_BACKGROUND),
        ));
    }
    fn card_header(&mut self, label: &str) {
        self.emit(RichLine {
            line: Line::from(vec![
                Span::styled("╭─ ", Style::default().fg(BORDER)),
                Span::styled(label.to_owned(), Style::default().fg(MUTED)),
            ])
            .style(Style::default().bg(CODE_BACKGROUND)),
            anchors: Vec::new(),
        });
    }
    fn finish(mut self, trailing_newline: bool) -> RenderedDocument {
        self.flush(false);
        if trailing_newline || self.lines.is_empty() {
            self.push(RichLine::default());
        }
        let mut lines = Vec::new();
        let mut footnotes = Vec::new();
        if self.omitted {
            let marker =
                RichLine::plain(ui_text::EARLIER_LINES_OMITTED, Style::default().fg(MUTED));
            lines.extend(
                wrap_line(marker, self.width, MAX_LINES)
                    .into_iter()
                    .map(|line| line.line),
            );
        }
        let skip = self
            .lines
            .len()
            .saturating_sub(MAX_LINES.saturating_sub(lines.len()));
        for rich in self.lines.into_iter().skip(skip) {
            let text = rich.text();
            for anchor in rich.anchors {
                if footnotes.len() >= MAX_ANCHORS {
                    break;
                }
                if let (Some(prefix), Some(marker)) = (
                    text.get(..anchor.offset),
                    text.get(anchor.offset..anchor.offset.saturating_add(anchor.length)),
                ) {
                    footnotes.push(FootnoteAnchor {
                        key: anchor.key,
                        number: anchor.number,
                        line: lines.len(),
                        column: prefix.width(),
                        width: marker.width(),
                        definition: anchor.definition,
                        occurrence: anchor.occurrence,
                    });
                }
            }
            lines.push(rich.line);
        }
        RenderedDocument { lines, footnotes }
    }
}
impl RichLine {
    pub(super) fn plain(text: impl Into<String>, style: Style) -> Self {
        Self {
            line: Line::styled(safe_text(&text.into()), style),
            anchors: Vec::new(),
        }
    }
    pub(super) fn text(&self) -> String {
        self.line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }
    pub(super) fn byte_len(&self) -> usize {
        self.line.spans.iter().map(|span| span.content.len()).sum()
    }
    pub(super) fn append(&mut self, mut other: Self) {
        let offset = self.byte_len();
        for anchor in &mut other.anchors {
            anchor.offset = anchor.offset.saturating_add(offset);
        }
        for span in other.line.spans {
            append_span(
                &mut self.line.spans,
                &span.content,
                other.line.style.patch(span.style),
            );
        }
        self.anchors.extend(other.anchors);
        self.line.style = self.line.style.patch(other.line.style);
    }
}

/// Retain at most the last limit physical rows and their semantic fragments.
pub(super) fn wrap_line(rich: RichLine, width: usize, limit: usize) -> Vec<RichLine> {
    let mut rows = VecDeque::new();
    let mut count = 0;
    let mut fragments: BTreeMap<usize, Vec<ByteAnchor>> = BTreeMap::new();
    let mut anchor_index = 0;
    append_wrapped_line_observed(rich.line, width, &mut rows, &mut count, limit, |position| {
        while anchor_index < rich.anchors.len()
            && rich.anchors[anchor_index]
                .offset
                .saturating_add(rich.anchors[anchor_index].length)
                <= position.source_offset
        {
            anchor_index += 1;
        }
        let Some(anchor) = rich.anchors.get(anchor_index) else {
            return;
        };
        if position.source_offset < anchor.offset {
            return;
        }
        let row = fragments.entry(position.row).or_default();
        if let Some(last) = row.last_mut()
            && last.key == anchor.key
            && last.definition == anchor.definition
            && last.occurrence == anchor.occurrence
            && last.offset + last.length == position.byte_offset
        {
            last.length += position.bytes;
        } else {
            let mut fragment = anchor.clone();
            fragment.offset = position.byte_offset;
            fragment.length = position.bytes;
            row.push(fragment);
        }
    });
    let first = count.saturating_sub(rows.len());
    rows.into_iter()
        .enumerate()
        .map(|(index, line)| RichLine {
            line,
            anchors: fragments.remove(&(first + index)).unwrap_or_default(),
        })
        .collect()
}
fn base_style() -> Style {
    Style::default().fg(FOREGROUND)
}
fn footnote_style(style: Style) -> Style {
    style
        .fg(HEADING)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
}
fn math_fits(rendered: &str, width: usize) -> bool {
    rendered.lines().all(|line| line.width() <= width)
        && rendered
            .graphemes(true)
            .all(|cluster| cluster.len() <= crate::editor::MAX_GRAPHEME_BYTES)
}
fn closed_fence(source: &str) -> bool {
    let mut lines = source.lines();
    let Some(first) = lines.next() else {
        return false;
    };
    let first = first.trim_start_matches(' ');
    let Some(marker @ (96 | 126)) = first.as_bytes().first().copied() else {
        return false;
    };
    let length = first.bytes().take_while(|byte| *byte == marker).count();
    let Some(last) = lines.rfind(|line| !line.trim().is_empty()) else {
        return false;
    };
    let last = last.trim();
    let closing = last.bytes().take_while(|byte| *byte == marker).count();
    closing >= length && last[closing..].is_empty()
}
fn fenced_language(source: &str) -> String {
    let first = source.lines().next().unwrap_or("");
    let Some(start) = first.bytes().position(|byte| matches!(byte, 96 | 126)) else {
        return ui_text::CODE_LABEL.into();
    };
    let marker = first.as_bytes()[start];
    let length = first[start..]
        .bytes()
        .take_while(|byte| *byte == marker)
        .count();
    first[start + length..]
        .split_whitespace()
        .next()
        .unwrap_or(ui_text::CODE_LABEL)
        .to_owned()
}
fn source_fallback(text: &str, width: usize, message: &str) -> RenderedDocument {
    let header = wrap_line(
        RichLine::plain(message, Style::default().fg(MUTED)),
        width,
        MAX_LINES,
    );
    let mut rows = VecDeque::new();
    let mut count = 0;
    for line in text.split('\n') {
        super::append_wrapped_line(
            Line::styled(line.to_owned(), base_style()),
            width,
            &mut rows,
            &mut count,
            MAX_LINES.saturating_sub(header.len()),
        );
    }
    let mut lines = header.into_iter().map(|line| line.line).collect::<Vec<_>>();
    lines.extend(rows);
    RenderedDocument {
        lines,
        footnotes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::safe_text;

    fn text(document: &RenderedDocument) -> String {
        document
            .lines
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

    fn styled(document: &RenderedDocument, needle: &str, modifier: Modifier) -> bool {
        document
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.content.contains(needle) && span.style.add_modifier.contains(modifier))
    }

    #[test]
    fn paragraphs_atx_and_setext_headings_and_rules_are_distinct() {
        let source = "# One\n## Two\n### Three\n#### Four\n##### Five\n###### Six\n\nSetext one\n===\n\nSetext two\n---\n\nOrdinary paragraph wraps to its available width.\n\n---\n\nAfter rule.";
        let rendered = render_document(source, 80);
        for heading in [
            "One",
            "Two",
            "Three",
            "Four",
            "Five",
            "Six",
            "Setext one",
            "Setext two",
        ] {
            assert!(styled(&rendered, heading, Modifier::BOLD), "{heading}");
        }
        let plain = text(&rendered);
        assert!(plain.contains(&"─".repeat(80)));
        assert!(!plain.contains("###"));
        let narrow = render_document("中文 and e\u{301} wrap without splitting graphemes.", 8);
        assert!(narrow.lines.iter().all(|line| line.width() <= 8));
        assert!(text(&narrow).contains("e\u{301}"));
    }

    #[test]
    fn nested_emphasis_strike_and_escaped_delimiters_follow_commonmark() {
        let source =
            "**bold** *italic* ***both*** ~~deleted~~ __strong__ \\*literal\\* \x60**code**\x60";
        let rendered = render_document(source, 160);
        assert!(styled(&rendered, "bold", Modifier::BOLD));
        assert!(styled(&rendered, "italic", Modifier::ITALIC));
        assert!(styled(&rendered, "both", Modifier::BOLD | Modifier::ITALIC));
        assert!(styled(&rendered, "deleted", Modifier::CROSSED_OUT));
        assert!(text(&rendered).contains("*literal* **code**"));
        assert!(
            rendered
                .lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.content == "**code**" && span.style.bg == Some(CODE_BACKGROUND))
        );
        let partial = render_document("unfinished **bold and \x60code", 80);
        assert_eq!(text(&partial), "unfinished **bold and \x60code");
    }

    #[test]
    fn nested_lists_tasks_and_quotes_keep_their_structural_prefixes() {
        let source = "- parent\n  - child\n    - grandchild\n- [x] done\n- [ ] pending\n\n3. third\n   1. nested\n4. fourth\n\n> outer\n> > inner\n> > > deepest";
        let plain = text(&render_document(source, 80));
        for expected in [
            "- parent",
            "  - child",
            "    - grandchild",
            "- [x] done",
            "- [ ] pending",
            "3. third",
            "   1. nested",
            "4. fourth",
            "│ outer",
            "│ │ inner",
            "│ │ │ deepest",
        ] {
            assert!(
                plain.contains(expected),
                "{expected:?} missing in {plain:?}"
            );
        }
    }

    #[test]
    fn code_blocks_highlight_languages_and_keep_html_as_source() {
        let source = "\x60\x60\x60rust\nlet value = \"**literal**\";\n\x60\x60\x60\n\n    const plain = 42;\n\n<span class=\"x\">tag</span>\n\n<section>\n**raw HTML block**\n</section>";
        let rendered = render_document(source, 100);
        let plain = text(&rendered);
        for raw in [
            "let value = \"**literal**\";",
            "const plain = 42;",
            "<span class=\"x\">tag</span>",
            "<section>",
            "**raw HTML block**",
            "</section>",
        ] {
            assert!(plain.contains(raw), "{raw:?} missing in {plain:?}");
        }
        let colors = rendered
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .filter(|span| span.content.contains("let") || span.content.contains("literal"))
            .filter_map(|span| span.style.fg)
            .collect::<Vec<_>>();
        assert!(colors.len() >= 2);
        assert_ne!(colors[0], colors[1]);
    }

    #[test]
    fn links_images_reference_links_and_autolinks_keep_visible_destinations() {
        let source = "[**Guide**](https://example.test/docs \"Title\") ![diagram](https://example.test/a.png)\n\n<https://example.test/auto> <reader@example.test>\n\nBare https://example.test/a*b*c_(one).\n\n[reference][id]\n\n[id]: https://example.test/ref";
        let rendered = render_document(source, 240);
        let plain = text(&rendered);
        for expected in [
            "Guide (https://example.test/docs)",
            "Image · diagram (https://example.test/a.png)",
            "https://example.test/auto",
            "reader@example.test",
            "https://example.test/a*b*c_(one)",
            "reference (https://example.test/ref)",
        ] {
            assert!(
                plain.contains(expected),
                "{expected:?} missing in {plain:?}"
            );
        }
        assert!(styled(
            &rendered,
            "Guide",
            Modifier::BOLD | Modifier::UNDERLINED
        ));
        assert!(
            rendered
                .lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.content == "Guide" && span.style.fg == Some(BLUE))
        );
        assert!(!plain.contains("\"Title\""));
    }

    #[test]
    fn footnotes_number_references_before_definitions_and_only_mark_semantic_tokens() {
        let source = "[^b]: Defined first.\n\nFirst [^a], second [^b], repeated [^a], literal \x60[^a]\x60, undefined [^missing].\n\n[^a]: Defined second.";
        let rendered = render_document(source, 120);
        let references = rendered
            .footnotes
            .iter()
            .filter(|anchor| !anchor.definition)
            .collect::<Vec<_>>();
        assert_eq!(references.len(), 3);
        assert_eq!(
            (
                references[0].key.as_str(),
                references[0].number,
                references[0].occurrence
            ),
            ("a", 1, 0)
        );
        assert_eq!(
            (
                references[1].key.as_str(),
                references[1].number,
                references[1].occurrence
            ),
            ("b", 2, 0)
        );
        assert_eq!(
            (
                references[2].key.as_str(),
                references[2].number,
                references[2].occurrence
            ),
            ("a", 1, 1)
        );
        assert_eq!(
            rendered
                .footnotes
                .iter()
                .filter(|anchor| anchor.definition)
                .count(),
            2
        );
        let plain = text(&rendered);
        assert!(plain.contains("literal [^a], undefined [^missing]"));
        assert!(plain.contains("[2] Defined first."));
        assert!(plain.contains("[1] Defined second."));
    }

    #[test]
    fn table_inline_syntax_math_and_footnotes_use_the_same_renderer() {
        let source = "| Feature | Detail |\n| --- | --- |\n| ***Strong*** | ~~old~~ $x^2$ [^n] |\n| \x60left\\|right\x60 | [Guide](https://example.test) |\n\n[^n]: Table note.";
        for width in [20, 40, 100] {
            let rendered = render_document(source, width);
            assert!(rendered.lines.iter().all(|line| line.width() <= width));
            let plain = text(&rendered);
            assert!(plain.contains("Strong"));
            assert!(plain.contains("x²"));
            let first_column = rendered
                .lines
                .iter()
                .filter_map(|line| {
                    let text = line
                        .spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>();
                    text.starts_with('│')
                        .then(|| text.split('│').nth(1).unwrap_or("").trim().to_owned())
                })
                .collect::<String>();
            assert!(first_column.contains("left|right"), "{plain}");
            assert!(
                rendered
                    .footnotes
                    .iter()
                    .any(|anchor| !anchor.definition && anchor.key == "n")
            );
            assert!(
                rendered
                    .footnotes
                    .iter()
                    .any(|anchor| anchor.definition && anchor.key == "n")
            );
        }
    }

    #[test]
    fn footnote_fragments_remain_valid_at_width_one_and_with_unicode_prefixes() {
        let source = "中文e\u{301} [^note] tail.\n\n[^note]: 中文 explanation.";
        for width in [1, 2, 3, 8, 40] {
            let rendered = render_document(source, width);
            assert!(rendered.lines.iter().all(|line| line.width() <= width));
            assert!(rendered.footnotes.iter().any(|anchor| anchor.definition));
            for anchor in &rendered.footnotes {
                assert!(anchor.line < rendered.lines.len());
                assert!(anchor.column + anchor.width <= width);
                assert!(anchor.width > 0);
            }
            assert_eq!(
                rendered
                    .footnotes
                    .iter()
                    .filter(|anchor| !anchor.definition)
                    .map(|anchor| anchor.width)
                    .sum::<usize>(),
                3
            );
        }
    }

    #[test]
    fn all_math_delimiters_and_math_fences_share_strict_source_fallback() {
        let source = "$x^2$ and \\(y_1\\)\n\n$$\\alpha + \\beta$$\n\n\\[\\sqrt{x}\\]\n\n\x60\x60\x60latex\nx^2\n\x60\x60\x60\n\n\x60\x60\x60math\n\\unknown{x}\n\x60\x60\x60\n\n\\(x_q\\)";
        let plain = text(&render_document(source, 100));
        for expected in [
            "x²",
            "y₁",
            "α + β",
            "√",
            "source fallback",
            "\\unknown{x}",
            "\\(x_q\\)",
        ] {
            assert!(
                plain.contains(expected),
                "{expected:?} missing in {plain:?}"
            );
        }
    }

    #[test]
    fn normalization_does_not_rewrite_native_math_or_code_contents() {
        let source = "$\\text{https://example.test/a*b}$\n\n$$\\unknown{https://example.test/x}$$\n\n\x60\\(x^2\\) https://example.test/a*b\x60";
        let normalized = normalize(source);
        assert_eq!(normalized.text, source);
        let rendered = text(&render_document(source, 180));
        assert!(rendered.contains("https://example.test/a*b"));
        assert!(rendered.contains("$$\\unknown{https://example.test/x}$$"));
        assert!(rendered.contains("\\(x^2\\) https://example.test/a*b"));
        let source = "\\(".repeat(30_000);
        assert!(normalize(&source).incomplete_math);
        assert!(text(&render_document(&source, 80)).contains("math source fallback"));
    }

    #[test]
    fn mermaid_valid_blocks_render_and_invalid_or_incomplete_blocks_keep_source() {
        let valid = "\x60\x60\x60mermaid\nflowchart LR\nA[Start] --> B[Done]\n\x60\x60\x60";
        let rendered = text(&render_document(valid, 100));
        assert!(rendered.contains("Start") && rendered.contains("Done"));
        assert!(!rendered.contains("source fallback"));
        for source in [
            "\x60\x60\x60mermaid\nflowchart TD\nA[unfinished --> B\n\x60\x60\x60",
            "\x60\x60\x60mermaid\nflowchart LR\nA --> B",
            "\x60\x60\x60latex\nx^2",
        ] {
            let rendered = text(&render_document(source, 80));
            assert!(rendered.contains("source fallback"), "{rendered}");
            assert!(rendered.contains(source.lines().nth(1).unwrap()));
        }
    }

    #[test]
    fn sanitized_controls_and_structure_budgets_never_generate_terminal_sequences() {
        let hostile = safe_text(
            "## H\u{1b}]2;bad\u{7}\n\n[x](https://example.test/\u{202e}path)\n\n| H | V |\n| --- | --- |\n| note | [^n] |\n\n[^n]: note\u{9b}x",
        );
        let rendered = text(&render_document(&hostile, 60));
        assert!(
            !rendered
                .chars()
                .any(|character| character.is_control() && character != '\n')
        );
        assert!(!rendered.contains('\u{202e}'));
        let source = "> ".repeat(MAX_DEPTH + 1) + "nested";
        assert!(text(&render_document(&source, 80)).contains("structure budget exceeded"));
        let source = "a".repeat(MAX_SOURCE_BYTES + 1);
        let document = render_document(&source, 80);
        assert!(text(&document).starts_with("[Markdown source budget exceeded]"));
        assert!(document.lines.len() <= MAX_LINES);
        assert!(document.footnotes.is_empty());
    }

    #[test]
    fn character_entities_preserve_their_source_without_creating_controls() {
        let source = "&#x1b;]2;title&#7; &#x9b;31m &#x202e;hidden\n\n[x](https://example.test/&#x1b;path)\n\n\x60\x60\x60&#x1b;language\nsource\n\x60\x60\x60";
        let plain = text(&render_document(source, 160));
        assert!(
            !plain
                .chars()
                .any(|character| character.is_control() && character != '\n')
        );
        assert!(!plain.contains('\u{202e}'));
        assert!(plain.contains("&#x1b;"));
        assert!(plain.contains("&#x202e;"));
        let source = format!(
            "{}value{}",
            "![".repeat(100),
            "](https://example.test)".repeat(100)
        );
        assert!(text(&render_document(&source, 80)).contains("structure budget exceeded"));
    }

    #[test]
    fn code_preserves_intentional_final_blank_rows() {
        let source = "\x60\x60\x60text\nfirst\n\n\x60\x60\x60";
        let rendered = render_document(source, 80);
        assert_eq!(
            rendered
                .lines
                .iter()
                .map(|line| line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>())
                .collect::<Vec<_>>(),
            ["╭─ text", "first", "", "╰─"]
        );
    }

    #[test]
    fn non_fitting_block_math_retains_complete_source_instead_of_wrapping_a_matrix() {
        let source = "\x60\x60\x60latex\n\\begin{pmatrix}long & labels \\\\ other & cells\\end{pmatrix}\n\x60\x60\x60";
        let wide = text(&render_document(source, 80));
        assert!(wide.contains("long") && !wide.contains("source fallback"));
        let narrow = render_document(source, 8);
        assert!(narrow.lines.iter().all(|line| line.width() <= 8));
        let joined = text(&narrow).replace('\n', "");
        assert!(joined.contains("source fallback"));
        assert!(joined.contains("\\begin{pmatrix}"));
    }

    #[test]
    fn display_omission_markers_are_wrapped_and_remain_inside_the_document_budget() {
        let rendered = render_document(&"x".repeat(MAX_LINES * 2), 1);
        assert!(rendered.lines.len() <= MAX_LINES);
        assert!(rendered.lines.iter().all(|line| line.width() <= 1));
        assert!(
            text(&rendered)
                .replace('\n', "")
                .contains("display lines omitted")
        );
        let rendered = render_document("\\(unfinished", 1);
        assert!(rendered.lines.iter().all(|line| line.width() <= 1));
        assert!(
            text(&rendered)
                .replace('\n', "")
                .contains("math source fallback")
        );
    }

    #[test]
    fn entity_rendering_cannot_restore_a_credential_hidden_from_the_upstream_filter() {
        for source in [
            "s&#x6b;-SYNTHETIC",
            "[s&#x6b;-SYNTHETIC](https://e.test/s&#x6b;-SYNTHETIC)",
            "![s&#x6b;-SYNTHETIC](https://e.test/s&#x6b;-SYNTHETIC)",
            "<https://e.test/s&#x6b;-SYNTHETIC>",
            "https://e.test/s&#x6b;-SYNTHETIC",
            "[reference][id]\n\n[id]: https://e.test/s&#x6b;-SYNTHETIC",
            "\x60s&#x6b;-SYNTHETIC\x60",
            "\x60\x60\x60s&#x6b;-SYNTHETIC\ns&#x6b;-SYNTHETIC\n\x60\x60\x60",
            "<div data-secret=\"s&#x6b;-SYNTHETIC\">s&#x6b;-SYNTHETIC</div>",
            "$\\text{s&#x6b;-SYNTHETIC}$",
        ] {
            let rendered = text(&render_document(source, 240));
            assert!(
                !rendered.contains("sk-SYNTHETIC"),
                "{source:?} decoded to {rendered:?}"
            );
            assert!(
                rendered.contains("s&#x6b;-SYNTHETIC"),
                "{source:?} changed to {rendered:?}"
            );
        }
        let bare = "https://e.test/?a=1&b=2";
        assert_eq!(text(&render_document(bare, 120)), bare);
        assert_eq!(
            text(&render_document("[x](https://e.test/?a=1&amp;b=2)", 120)),
            "x (https://e.test/?a=1&amp;b=2)"
        );
    }

    #[test]
    fn unicode_footnote_names_use_the_parsers_full_case_folding() {
        let source = "First [^SS], second [^ς].\n\n[^ß]: sharp s.\n\n[^Σ]: sigma.";
        let rendered = render_document(source, 120);
        let refs = rendered
            .footnotes
            .iter()
            .filter(|anchor| !anchor.definition)
            .collect::<Vec<_>>();
        assert_eq!(refs.len(), 2);
        assert_eq!((refs[0].key.as_str(), refs[0].number), ("ss", 1));
        assert_eq!((refs[1].key.as_str(), refs[1].number), ("σ", 2));
        assert_eq!(
            rendered
                .footnotes
                .iter()
                .filter(|anchor| anchor.definition)
                .count(),
            2
        );
    }

    #[test]
    fn explicit_math_in_link_labels_is_rendered_without_changing_url_or_code() {
        let source = "[formula \\(x^2\\)](https://e.test/a\\(literal\\)) and [\x60\\(literal\\)\x60](https://e.test/code)";
        let rendered = render_document(source, 240);
        let plain = text(&rendered);
        assert!(
            plain.contains("formula x² (https://e.test/a(literal))"),
            "{plain}"
        );
        assert!(
            plain.contains("\\(literal\\) (https://e.test/code)"),
            "{plain}"
        );
        assert!(
            rendered
                .lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.content.contains("x²")
                    && span.style.fg == Some(BLUE)
                    && span.style.add_modifier.contains(Modifier::UNDERLINED))
        );
    }

    #[test]
    fn quotes_and_code_headers_retain_the_shared_theme() {
        let document = render_document("> quote\n> > nested quote\n\n```rust\nlet x = 1;\n```", 80);
        for line in &document.lines {
            for span in &line.spans {
                if span.content.contains("quote") {
                    assert_eq!(span.style.fg, Some(MUTED));
                }
                if span.content.starts_with("╭─") {
                    assert_eq!(span.style.fg, Some(BORDER));
                }
                if span.content == "rust" {
                    assert_eq!(span.style.fg, Some(MUTED));
                }
            }
        }
        assert!(
            document
                .lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.content.contains("let") && span.style.fg != Some(CODE_FOREGROUND))
        );
    }
}
