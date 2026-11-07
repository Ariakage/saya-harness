//! Pure, bounded TUI state and rendering. Runtime effects belong to the controller.
use crate::editor::{Editor, prefix_boundary, safe_text};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;
pub const MAX_TRANSCRIPT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_MESSAGES: usize = 512;
const MAX_RENDER_LINES: usize = 10_000;
const ACCENT: Color = Color::Rgb(127, 212, 226);
const MUTED: Color = Color::Rgb(147, 155, 174);
const BACKGROUND: Color = Color::Rgb(20, 23, 31);
const FOREGROUND: Color = Color::Rgb(225, 229, 238);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Tool,
    System,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub id: u64,
    pub role: Role,
    pub label: String,
    pub text: String,
    pub streaming: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct Details {
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Debug)]
pub struct State {
    pub workspace: String,
    pub profile: String,
    pub model: String,
    pub mode: String,
    pub permissions: String,
    pub status: String,
    pub pending_mode: Option<String>,
    pub usage: String,
    pub editor: Editor,
    pub messages: VecDeque<Message>,
    pub details: Option<Details>,
    /// Visual line offset from the top of the current details pane.
    pub details_scroll: usize,
    /// Visual lines above the live bottom. Zero follows streamed output.
    pub scroll: usize,
    pub dropped_messages: usize,
    next_id: u64,
    last_width: u16,
    last_line_count: usize,
}

impl Default for State {
    fn default() -> Self {
        Self {
            workspace: ".".into(),
            profile: "unconfigured".into(),
            model: String::new(),
            mode: "WORK".into(),
            permissions: "auto".into(),
            status: "Ready".into(),
            pending_mode: None,
            usage: String::new(),
            editor: Editor::default(),
            messages: VecDeque::new(),
            details: None,
            details_scroll: 0,
            scroll: 0,
            dropped_messages: 0,
            next_id: 1,
            last_width: 0,
            last_line_count: 0,
        }
    }
}

impl State {
    /// Add visible content and return a stable ID, retained across viewport eviction.
    /// Model/provider secrets must already have been removed by the controller.
    pub fn push_message(
        &mut self,
        role: Role,
        label: impl Into<String>,
        text: impl AsRef<str>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let source = text.as_ref();
        let end = prefix_boundary(source, MAX_MESSAGE_BYTES);
        let label = label.into();
        let label = label[..prefix_boundary(&label, 256)].to_owned();
        self.messages.push_back(Message {
            id,
            role,
            label,
            text: source[..end].to_owned(),
            streaming: false,
            truncated: end < source.len(),
        });
        self.enforce_limits();
        id
    }

    /// Append a stream delta. The ID may have left the bounded viewport.
    /// The return value indicates that the message was present.
    pub fn append_message(&mut self, id: u64, delta: &str) -> bool {
        let Some(message) = self.messages.iter_mut().find(|message| message.id == id) else {
            return false;
        };
        let remaining = MAX_MESSAGE_BYTES.saturating_sub(message.text.len());
        let end = prefix_boundary(delta, remaining);
        message.text.push_str(&delta[..end]);
        message.truncated |= end < delta.len();
        message.streaming = true;
        self.enforce_limits();
        true
    }

    /// Replace a retained message with its final canonical text.
    pub fn replace_message(&mut self, id: u64, text: impl AsRef<str>) -> bool {
        let Some(message) = self.messages.iter_mut().find(|message| message.id == id) else {
            return false;
        };
        let source = text.as_ref();
        let end = prefix_boundary(source, MAX_MESSAGE_BYTES);
        message.text = source[..end].to_owned();
        message.truncated = end < source.len();
        message.streaming = false;
        self.enforce_limits();
        true
    }

    /// Reset the viewport for a session switch while keeping message IDs distinct.
    pub fn clear_messages(&mut self) {
        self.messages.clear();
        self.scroll = 0;
        self.details_scroll = 0;
        self.dropped_messages = 0;
        self.last_width = 0;
        self.last_line_count = 0;
    }

    pub fn finish_message(&mut self, id: u64) {
        if let Some(message) = self.messages.iter_mut().find(|message| message.id == id) {
            message.streaming = false;
        }
    }

    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll = self.scroll.saturating_add(lines);
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll = self.scroll.saturating_sub(lines);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll = 0;
    }

    /// Limit direct state population as well as streamed updates.
    pub fn enforce_limits(&mut self) {
        for message in &mut self.messages {
            message.label.truncate(prefix_boundary(&message.label, 256));
            if message.text.len() > MAX_MESSAGE_BYTES {
                let end = prefix_boundary(&message.text, MAX_MESSAGE_BYTES);
                message.text.truncate(end);
                message.truncated = true;
            }
        }
        let mut retained = self
            .messages
            .iter()
            .map(|message| message.text.len() + message.label.len())
            .sum::<usize>();
        while self.messages.len() > MAX_MESSAGES || retained > MAX_TRANSCRIPT_BYTES {
            let Some(message) = self.messages.pop_front() else {
                break;
            };
            retained = retained.saturating_sub(message.text.len() + message.label.len());
            self.dropped_messages = self.dropped_messages.saturating_add(1);
        }
    }
}

/// Render a state snapshot. Terminal controls are displayed as escaped text.
/// This function performs no model, storage or terminal I/O.
pub fn draw(frame: &mut Frame<'_>, state: &mut State) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().fg(FOREGROUND).bg(BACKGROUND)),
        area,
    );
    if area.width < 20 || area.height < 6 {
        frame.render_widget(
            Paragraph::new("Saya: enlarge terminal to at least 20 x 6"),
            area,
        );
        return;
    }
    state.enforce_limits();
    let input_lines = state
        .editor
        .text()
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    let input_height = (input_lines + 2)
        .clamp(3, 8)
        .min(usize::from(area.height / 3).max(3)) as u16;
    let areas = Layout::vertical([
        Constraint::Length(if area.height < 12 { 2 } else { 3 }),
        Constraint::Min(1),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .split(area);
    draw_header(frame, state, areas[0]);
    let body = areas[1];
    if state.details.is_some() && body.width >= 100 {
        let columns = Layout::horizontal([
            Constraint::Min(50),
            Constraint::Length((body.width / 3).clamp(32, 48)),
        ])
        .split(body);
        draw_transcript(frame, state, columns[0]);
        if let Some(details) = &state.details {
            draw_details(frame, details, &mut state.details_scroll, columns[1]);
        }
    } else if let Some(details) = &state.details {
        draw_details(frame, details, &mut state.details_scroll, body);
    } else {
        draw_transcript(frame, state, body);
    }
    draw_editor(frame, state, areas[2]);
    let footer = if state.details.is_some() {
        "PgUp/PgDn details  Esc back  Ctrl+P commands  Ctrl+C stop"
    } else if state.scroll == 0 {
        "Enter send  Alt+Enter/Ctrl+J newline  Ctrl+P commands  Ctrl+C stop  Ctrl+D exit"
    } else {
        "PgUp/PgDn scroll  End follow output  Ctrl+P commands  Ctrl+C stop"
    };
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().fg(MUTED)),
        areas[3],
    );
}

fn draw_header(frame: &mut Frame<'_>, state: &State, area: Rect) {
    let mode = if let Some(pending) = &state.pending_mode {
        format!("{} -> {}", state.mode, pending)
    } else {
        state.mode.clone()
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            " SAYA ",
            Style::default()
                .fg(BACKGROUND)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "  {}  |  {}  |  {}",
                safe_line(&mode),
                safe_line(&state.permissions),
                safe_line(&state.status)
            ),
            Style::default().fg(FOREGROUND),
        ),
    ])];
    lines.push(Line::styled(
        format!(
            " {}  ·  {} / {}",
            safe_line(&state.workspace),
            safe_line(&state.profile),
            safe_line(&state.model)
        ),
        Style::default().fg(MUTED),
    ));
    if !state.usage.is_empty() && area.height >= 3 {
        lines.push(Line::styled(
            format!(" {}", safe_line(&state.usage)),
            Style::default().fg(MUTED),
        ));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn safe_line(text: &str) -> String {
    safe_text(text).replace('\n', "\\n")
}

fn role_style(role: Role) -> Style {
    Style::default().fg(match role {
        Role::User => Color::Rgb(180, 194, 255),
        Role::Assistant => ACCENT,
        Role::Tool => Color::Rgb(229, 190, 130),
        Role::System => MUTED,
    })
}

fn draw_transcript(frame: &mut Frame<'_>, state: &mut State, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(65, 73, 92)))
        .title(" Conversation ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let width = usize::from(inner.width);
    let mut lines = VecDeque::new();
    let mut line_count = 0;
    if state.dropped_messages > 0 {
        append_wrapped(
            &format!(
                "[{} earlier viewport messages omitted]",
                state.dropped_messages
            ),
            width,
            Style::default().fg(MUTED),
            &mut lines,
            &mut line_count,
        );
    }
    for message in &state.messages {
        let label = if message.label.is_empty() {
            match message.role {
                Role::User => "You",
                Role::Assistant => "Saya",
                Role::Tool => "Tool",
                Role::System => "System",
            }
        } else {
            &message.label
        };
        let mut heading = safe_text(label);
        if message.streaming {
            heading.push_str(" · streaming");
        }
        if message.truncated {
            heading.push_str(" · truncated");
        }
        append_wrapped(
            &heading,
            width,
            role_style(message.role).add_modifier(Modifier::BOLD),
            &mut lines,
            &mut line_count,
        );
        let mut code = false;
        for line in safe_text(&message.text).split('\n') {
            if line.trim_start().starts_with("```") {
                code = !code;
            }
            let style = if code || line.trim_start().starts_with("```") {
                Style::default()
                    .fg(Color::Rgb(203, 211, 225))
                    .bg(Color::Rgb(29, 34, 46))
            } else {
                Style::default().fg(FOREGROUND)
            };
            append_wrapped(line, width, style, &mut lines, &mut line_count);
        }
        if message.truncated {
            append_wrapped(
                "[display truncated]",
                width,
                Style::default().fg(Color::Yellow),
                &mut lines,
                &mut line_count,
            );
        }
        append_wrapped("", width, Style::default(), &mut lines, &mut line_count);
    }
    if line_count > MAX_RENDER_LINES {
        lines.push_front(Line::styled(
            "[earlier display lines omitted]",
            Style::default().fg(MUTED),
        ));
    }
    if state.scroll > 0 && state.last_width == inner.width && line_count > state.last_line_count {
        state.scroll = state
            .scroll
            .saturating_add(line_count - state.last_line_count);
    }
    state.last_width = inner.width;
    state.last_line_count = line_count;
    state.scroll = state
        .scroll
        .min(lines.len().saturating_sub(usize::from(inner.height)));
    let end = lines.len().saturating_sub(state.scroll);
    let start = end.saturating_sub(usize::from(inner.height));
    let visible = lines
        .into_iter()
        .skip(start)
        .take(end - start)
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible), inner);
}

fn draw_details(frame: &mut Frame<'_>, details: &Details, scroll: &mut usize, area: Rect) {
    let title = safe_line(&details.title[..prefix_boundary(&details.title, 256)]);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(area);
    if inner.width == 0 || inner.height == 0 {
        frame.render_widget(block, area);
        return;
    }
    let mut lines = VecDeque::new();
    let mut count = 0;
    let mut remaining = MAX_TRANSCRIPT_BYTES;
    let mut truncated = false;
    'sources: for source in &details.lines {
        let end = prefix_boundary(source, remaining);
        remaining -= end;
        let visible = safe_text(&source[..end]);
        for line in visible.split('\n') {
            let rows_left = MAX_RENDER_LINES.saturating_sub(lines.len());
            if rows_left == 0 {
                truncated = true;
                break 'sources;
            }
            let prefix = crop_display(line, 0, usize::from(inner.width) * rows_left);
            let line_truncated = prefix.len() < line.len();
            append_wrapped(
                &prefix,
                usize::from(inner.width),
                Style::default().fg(FOREGROUND),
                &mut lines,
                &mut count,
            );
            if line_truncated {
                truncated = true;
                break 'sources;
            }
        }
        if end < source.len() {
            truncated = true;
            break;
        }
    }
    if truncated {
        block = block.title(format!(" Truncated · {title} "));
        if lines.len() == MAX_RENDER_LINES {
            lines.pop_back();
        }
        lines.push_back(Line::styled(
            "[details display truncated]",
            Style::default().fg(Color::Yellow),
        ));
    }
    *scroll = (*scroll).min(lines.len().saturating_sub(usize::from(inner.height)));
    let visible = lines
        .into_iter()
        .skip(*scroll)
        .take(usize::from(inner.height))
        .collect::<Vec<_>>();
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(visible), inner);
}

fn draw_editor(frame: &mut Frame<'_>, state: &State, area: Rect) {
    let title = if state.editor.truncated {
        " Input · paste truncated at 64 KiB "
    } else {
        " Message "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let (cursor_row, cursor_column) = state.editor.cursor_position();
    let first_row = cursor_row.saturating_sub(usize::from(inner.height) - 1);
    let first_column = cursor_column.saturating_sub(usize::from(inner.width) - 1);
    let text = safe_text(state.editor.text());
    let lines = text
        .split('\n')
        .skip(first_row)
        .take(usize::from(inner.height))
        .map(|line| Line::from(crop_display(line, first_column, usize::from(inner.width))))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), inner);
    frame.set_cursor_position((
        inner.x + (cursor_column - first_column) as u16,
        inner.y + (cursor_row - first_row) as u16,
    ));
}

fn crop_display(text: &str, skip: usize, width: usize) -> String {
    let mut result = String::new();
    let mut column = 0;
    let mut retained = 0;
    for grapheme in text.graphemes(true) {
        let cells = UnicodeWidthStr::width(grapheme);
        let end = column + cells;
        if end > skip {
            let visible = cells.min(end - skip);
            if retained + visible > width {
                break;
            }
            if column < skip {
                result.push_str(&" ".repeat(visible));
            } else {
                result.push_str(grapheme);
            }
            retained += visible;
        }
        column = end;
    }
    result
}

fn append_wrapped(
    text: &str,
    width: usize,
    style: Style,
    lines: &mut VecDeque<Line<'static>>,
    count: &mut usize,
) {
    let mut current = String::new();
    let mut column = 0;
    for grapheme in text.graphemes(true) {
        if grapheme == "\n" {
            push_line(std::mem::take(&mut current), style, lines, count);
            column = 0;
            continue;
        }
        let cells = UnicodeWidthStr::width(grapheme);
        if column > 0 && column + cells > width {
            push_line(std::mem::take(&mut current), style, lines, count);
            column = 0;
        }
        if cells > width {
            current.push('�');
            column += 1;
        } else {
            current.push_str(grapheme);
            column += cells;
        }
    }
    push_line(current, style, lines, count);
}

fn push_line(text: String, style: Style, lines: &mut VecDeque<Line<'static>>, count: &mut usize) {
    *count = count.saturating_add(1);
    if lines.len() == MAX_RENDER_LINES {
        lines.pop_front();
    }
    lines.push_back(Line::styled(text, style));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn render(state: &mut State, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, state)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn split_utf8_streams_join_and_viewport_budget_is_explicit() {
        let mut state = State::default();
        let id = state.push_message(Role::Assistant, "Saya", "");
        for delta in ["hello ", "界", "👩‍💻", " e", "\u{301}"] {
            assert!(state.append_message(id, delta));
        }
        state.finish_message(id);
        assert_eq!(state.messages.back().unwrap().text, "hello 界👩‍💻 e\u{301}");
        for _ in 0..12 {
            state.push_message(Role::Tool, "Tool", "界".repeat(MAX_MESSAGE_BYTES));
        }
        assert!(
            state
                .messages
                .iter()
                .map(|message| message.text.len() + message.label.len())
                .sum::<usize>()
                <= MAX_TRANSCRIPT_BYTES
        );
        assert!(state.dropped_messages > 0);
        assert!(
            state
                .messages
                .iter()
                .all(|message| message.text.is_char_boundary(message.text.len()))
        );
        assert!(render(&mut state, 80, 24).contains("display truncated"));
    }

    #[test]
    fn canonical_replacement_and_session_reset_keep_stable_ids_and_budgets() {
        let mut state = State::default();
        let first = state.push_message(Role::Assistant, "Saya", "partial");
        state.append_message(first, " text");
        assert!(state.replace_message(first, "canonical answer"));
        assert_eq!(state.messages[0].text, "canonical answer");
        assert!(!state.messages[0].streaming);
        state.scroll_up(10);
        state.clear_messages();
        assert!(state.messages.is_empty());
        assert_eq!(state.scroll, 0);
        let next = state.push_message(Role::User, "You", "new session");
        assert!(next > first);
        assert!(!state.replace_message(first, "old result"));
    }

    #[test]
    fn narrow_frames_multiline_cursor_and_controls_render_without_escape_effects() {
        let mut state = State::default();
        state.push_message(
            Role::User,
            "You",
            "line\x1b[2J\u{9b}\u{202e}\n```rust\nlet x = 1;\n```",
        );
        state.editor.insert("hello\n界🌸\nlast");
        state.editor.move_up();
        let image = render(&mut state, 80, 24);
        assert!(image.contains("\\u{1b}"));
        assert!(!image.contains('\x1b'));
        assert!(!image.contains('\u{9b}'));
        for (width, height) in [(0, 0), (1, 1), (19, 5), (20, 6), (30, 10), (120, 30)] {
            render(&mut state, width, height);
        }
    }

    #[test]
    fn details_scroll_reaches_retained_long_fields_and_declares_display_limits() {
        let mut state = State {
            details: Some(Details {
                title: "Parameters".into(),
                lines: vec![
                    (0..100)
                        .map(|number| format!("row {number}\n"))
                        .collect::<String>(),
                ],
            }),
            ..State::default()
        };
        assert!(render(&mut state, 60, 20).contains("row 0"));
        state.details_scroll = 90;
        assert!(render(&mut state, 60, 20).contains("row 99"));
        state.details = Some(Details {
            title: "Oversized".into(),
            lines: vec!["x".repeat(MAX_TRANSCRIPT_BYTES + 1)],
        });
        state.details_scroll = 0;
        assert!(render(&mut state, 60, 20).contains("Truncated"));
    }

    #[test]
    fn scrollback_stays_anchored_while_new_lines_arrive_and_details_adapt() {
        let mut state = State::default();
        let id = state.push_message(
            Role::Assistant,
            "Saya",
            "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten",
        );
        render(&mut state, 40, 12);
        state.scroll_up(3);
        render(&mut state, 40, 12);
        let previous = state.scroll;
        state.append_message(id, "\neleven\ntwelve");
        render(&mut state, 40, 12);
        assert_eq!(state.scroll, previous + 2);
        state.details = Some(Details {
            title: "Tool details".into(),
            lines: vec!["call_id: fixture".into(), "state: unknown".into()],
        });
        assert!(render(&mut state, 120, 24).contains("Tool details"));
        assert!(render(&mut state, 40, 12).contains("unknown"));
    }
}
