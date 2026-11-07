//! Pure, bounded TUI state and rendering. Runtime effects belong to the controller.
use crate::{
    commands::Completion,
    editor::{Editor, prefix_boundary, safe_text},
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;
pub const MAX_TRANSCRIPT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_MESSAGES: usize = 512;
const MAX_RENDER_LINES: usize = 10_000;
const MAX_DETAIL_LINES: usize = 10_000;
const MAX_DETAIL_TITLE_BYTES: usize = 256;
const TRUNCATED_DETAIL_PREFIX: &str = "Truncated · ";
const DETAIL_TRUNCATION_NOTICE: &str = "[details display truncated]";
const ACCENT: Color = Color::Rgb(127, 212, 226);
const MUTED: Color = Color::Rgb(147, 155, 174);
const BACKGROUND: Color = Color::Rgb(20, 23, 31);
const FOREGROUND: Color = Color::Rgb(225, 229, 238);
const MAX_COMPLETION_ROWS: usize = 6;
const COMPACT_BACKGROUND: Color = Color::Black;
const INPUT_BACKGROUND: Color = Color::Rgb(52, 52, 52);
const COMPACT_BLUE: Color = Color::Rgb(47, 116, 196);
const EFFORT_COLOR: Color = Color::Rgb(255, 154, 40);

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
    pub session_title: String,
    pub workspace: String,
    pub profile: String,
    pub model: String,
    pub mode: String,
    pub permissions: String,
    pub status: String,
    pub pending_mode: Option<String>,
    pub usage: String,
    pub context_window_tokens: Option<u64>,
    pub context_input_tokens: Option<u64>,
    pub context_estimated: bool,
    pub thinking_effort: Option<String>,
    pub editor: Editor,
    pub completion: Completion,
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
            session_title: "New session".into(),
            workspace: ".".into(),
            profile: "unconfigured".into(),
            model: String::new(),
            mode: "WORK".into(),
            permissions: "auto".into(),
            status: "Ready".into(),
            pending_mode: None,
            usage: String::new(),
            context_window_tokens: None,
            context_input_tokens: None,
            context_estimated: false,
            thinking_effort: None,
            editor: Editor::default(),
            completion: Completion::default(),
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

    /// Retain a bounded details pane, including truncation metadata.
    /// Existing scroll position is preserved and clamped during rendering.
    pub fn set_details(&mut self, details: Details) {
        self.details = Some(details);
        self.enforce_details_limits();
    }

    fn enforce_details_limits(&mut self) {
        if let Some(details) = &mut self.details
            && details_need_limits(details)
        {
            bound_details(details);
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

fn details_need_limits(details: &Details) -> bool {
    if details.title.len() > MAX_DETAIL_TITLE_BYTES
        || details.title.capacity() > MAX_DETAIL_TITLE_BYTES
        || details.lines.capacity() > MAX_DETAIL_LINES
    {
        return true;
    }
    let mut bytes = 0usize;
    let mut allocated = 0usize;
    let mut rows = 0usize;
    for line in &details.lines {
        bytes = bytes.saturating_add(line.len());
        allocated = allocated.saturating_add(line.capacity());
        if bytes > MAX_TRANSCRIPT_BYTES || allocated > MAX_TRANSCRIPT_BYTES {
            return true;
        }
        rows = rows.saturating_add(1 + line.bytes().filter(|byte| *byte == b'\n').count());
        if rows > MAX_DETAIL_LINES {
            return true;
        }
    }
    false
}

pub(crate) fn bound_details(details: &mut Details) {
    let mut truncated = details.title.len() > MAX_DETAIL_TITLE_BYTES;
    details.title =
        details.title[..prefix_boundary(&details.title, MAX_DETAIL_TITLE_BYTES)].to_owned();
    let sources = std::mem::take(&mut details.lines);
    let mut bounded = Vec::with_capacity(sources.len().min(MAX_DETAIL_LINES));
    let mut retained = 0usize;
    'sources: for source in sources {
        let source_end = prefix_boundary(&source, MAX_TRANSCRIPT_BYTES.saturating_sub(retained));
        for line in source[..source_end].split('\n') {
            if bounded.len() == MAX_DETAIL_LINES {
                truncated = true;
                break 'sources;
            }
            bounded.push(line.to_owned());
            retained += line.len();
        }
        if source_end < source.len() {
            truncated = true;
            break;
        }
    }
    if truncated {
        while bounded.len() >= MAX_DETAIL_LINES
            || retained + DETAIL_TRUNCATION_NOTICE.len() > MAX_TRANSCRIPT_BYTES
        {
            let Some(tail) = bounded.pop() else { break };
            retained -= tail.len();
            let remaining =
                MAX_TRANSCRIPT_BYTES.saturating_sub(retained + DETAIL_TRUNCATION_NOTICE.len());
            if bounded.len() < MAX_DETAIL_LINES - 1 && remaining > 0 {
                let end = prefix_boundary(&tail, remaining.min(tail.len()));
                retained += end;
                bounded.push(tail[..end].to_owned());
            }
        }
        bounded.push(DETAIL_TRUNCATION_NOTICE.to_owned());
    }
    bounded.shrink_to_fit();
    details.lines = bounded;
}

/// Render a state snapshot. Terminal controls are displayed as escaped text.
/// This function performs no model, storage or terminal I/O.
pub fn draw(frame: &mut Frame<'_>, state: &mut State) {
    state.enforce_limits();
    state.enforce_details_limits();
    let area = frame.area();
    let compact = area.width < 100 || area.height < 24;
    frame.render_widget(
        Block::default().style(Style::default().fg(FOREGROUND).bg(if compact {
            COMPACT_BACKGROUND
        } else {
            BACKGROUND
        })),
        area,
    );
    if area.width < 20 || area.height < 6 {
        frame.render_widget(
            Paragraph::new("Saya: enlarge terminal to at least 20 x 6"),
            area,
        );
        return;
    }
    if compact {
        draw_compact(frame, state, area);
        return;
    }
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
        Constraint::Min(0),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .split(area);
    draw_header(frame, state, areas[0]);
    let body = areas[1];
    draw_body(frame, state, body, true);
    draw_editor(frame, state, areas[2], false);
    draw_completion(frame, state, body);
    let footer = if state.completion.visible() {
        "↑/↓ select  Tab/→ complete  Esc dismiss  Enter run"
    } else if state.details.is_some() {
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

fn draw_body(frame: &mut Frame<'_>, state: &mut State, body: Rect, framed: bool) {
    if state.details.is_some() && body.width >= 100 {
        let columns = Layout::horizontal([
            Constraint::Min(50),
            Constraint::Length((body.width / 3).clamp(32, 48)),
        ])
        .split(body);
        draw_transcript(frame, state, columns[0], framed);
        if let Some(details) = &state.details {
            draw_details(
                frame,
                details,
                &mut state.details_scroll,
                columns[1],
                framed,
            );
        }
    } else if let Some(details) = &state.details {
        draw_details(frame, details, &mut state.details_scroll, body, framed);
    } else {
        draw_transcript(frame, state, body, framed);
    }
}

fn draw_compact(frame: &mut Frame<'_>, state: &mut State, area: Rect) {
    let title = if area.width < 44 {
        Line::from("Saya Harness")
    } else {
        Line::from(vec![
            Span::raw("Saya Harness "),
            Span::styled(
                format!("(v{})", env!("CARGO_PKG_VERSION")),
                Style::default().fg(MUTED),
            ),
        ])
    };
    let hint = if state.completion.visible() {
        if area.width < 28 {
            "Tab/→"
        } else {
            "↑↓ Tab/→ Esc"
        }
    } else if area.width < 44 {
        "Ctrl+P"
    } else {
        "Ctrl+P commands"
    };
    draw_split_line(
        frame,
        title,
        Line::styled(hint, Style::default().fg(MUTED)),
        Rect::new(area.x, area.y, area.width, 1),
        12,
    );
    let outer = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(MUTED))
        .title(format!(" • {} ", safe_line(&state.workspace)));
    let inner = block.inner(outer);
    frame.render_widget(block, outer);
    let input_lines = state
        .editor
        .text()
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    let input_height = (input_lines + 2)
        .clamp(3, 5)
        .min(usize::from(inner.height.saturating_sub(1))) as u16;
    let rows = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(input_height),
    ])
    .split(inner);
    draw_body(frame, state, rows[0], false);
    draw_compact_status(frame, state, rows[1]);
    draw_editor(frame, state, rows[2], true);
    draw_completion(frame, state, rows[0]);
    draw_compact_footer(
        frame,
        state,
        Rect::new(inner.x, outer.bottom() - 1, inner.width, 1),
    );
}

fn draw_split_line(
    frame: &mut Frame<'_>,
    left: Line<'_>,
    right: Line<'_>,
    area: Rect,
    minimum_left: usize,
) {
    let right_width = right
        .width()
        .min(usize::from(area.width).saturating_sub(minimum_left + 1)) as u16;
    let left_width = area
        .width
        .saturating_sub(right_width + u16::from(right_width > 0));
    frame.render_widget(
        Paragraph::new(left),
        Rect::new(area.x, area.y, left_width, area.height),
    );
    frame.render_widget(
        Paragraph::new(right.right_aligned()),
        Rect::new(area.right() - right_width, area.y, right_width, area.height),
    );
}

fn draw_compact_status(frame: &mut Frame<'_>, state: &State, area: Rect) {
    let status = if area.width < 40 {
        match state.status.as_str() {
            "Awaiting approval" => "Approval",
            "Awaiting input" => "Input",
            "Cancelling" => "Cancel",
            "Completed" => "Done",
            _ => &state.status,
        }
    } else {
        &state.status
    };
    let permissions = if area.width < 48 {
        match state.permissions.as_str() {
            "Manual Approval" => "manual",
            "bypass permissions" => "bypass",
            _ => &state.permissions,
        }
    } else {
        &state.permissions
    };
    let ready = matches!(state.status.as_str(), "Ready" | "Completed");
    draw_split_line(
        frame,
        Line::from(vec![
            Span::styled(
                if ready { "✓ " } else { "● " },
                Style::default().fg(if ready { Color::Green } else { COMPACT_BLUE }),
            ),
            Span::raw(safe_line(status)),
        ]),
        Line::styled(safe_line(permissions), Style::default().fg(MUTED)),
        area,
        6,
    );
}

fn draw_compact_footer(frame: &mut Frame<'_>, state: &State, area: Rect) {
    let mode = if let Some(pending) = &state.pending_mode {
        format!("{}→{}", safe_line(&state.mode), safe_line(pending))
    } else {
        safe_line(&state.mode)
    };
    let minimum_left = UnicodeWidthStr::width(state.mode.as_str());
    let full_meter = context_meter(state, false);
    let meter = if full_meter.width() + minimum_left < usize::from(area.width) {
        full_meter
    } else {
        context_meter(state, true)
    };
    let left_width = usize::from(area.width).saturating_sub(meter.width() + 1);
    let effort = state
        .thinking_effort
        .as_deref()
        .map(safe_line)
        .unwrap_or_else(|| "—".into());
    let show_effort = area.width >= 32 && mode.width() + effort.width() + 2 < left_width;
    let model_width = left_width
        .saturating_sub(mode.width() + 1 + if show_effort { effort.width() + 1 } else { 0 });
    let mut left = Line::from(vec![
        Span::styled(mode, Style::default().fg(COMPACT_BLUE)),
        Span::raw(format!(
            " {}",
            crop_display(&safe_line(&state.model), 0, model_width)
        )),
    ]);
    if show_effort {
        left.spans.push(Span::styled(
            format!(" {effort}"),
            Style::default().fg(EFFORT_COLOR),
        ));
    }
    draw_split_line(
        frame,
        left,
        Line::styled(meter, Style::default().fg(MUTED)),
        area,
        minimum_left,
    );
}

fn context_meter(state: &State, short: bool) -> String {
    let window = state.context_window_tokens.filter(|window| *window > 0);
    let percent = state
        .context_input_tokens
        .zip(window)
        .map(|(input, window)| {
            if input > window {
                101
            } else {
                (u128::from(input) * 100 / u128::from(window)) as u64
            }
        });
    let ring = match percent {
        Some(0) | None => "○",
        Some(1..=25) => "◔",
        Some(26..=50) => "◑",
        Some(51..=75) => "◕",
        Some(_) => "●",
    };
    let percentage = match percent {
        Some(101) => ">100%".into(),
        Some(value) => format!("{value}%"),
        None => "—".into(),
    };
    let estimated = if state.context_estimated && state.context_input_tokens.is_some() {
        "~"
    } else {
        ""
    };
    if short {
        return format!("{ring} input {estimated}{percentage}");
    }
    let input = state
        .context_input_tokens
        .map(token_label)
        .unwrap_or_else(|| "—".into());
    let window = window.map(token_label).unwrap_or_else(|| "—".into());
    format!("{ring} input {estimated}{input}/{window} ({percentage})")
}

fn token_label(value: u64) -> String {
    for (divisor, suffix) in [
        (1_000_000_000_000, "T"),
        (1_000_000_000, "G"),
        (1_000_000, "M"),
        (1_000, "k"),
    ] {
        if value >= divisor {
            let whole = value / divisor;
            let decimal = value % divisor / (divisor / 10);
            return if decimal == 0 {
                format!("{whole}{suffix}")
            } else {
                format!("{whole}.{decimal}{suffix}")
            };
        }
    }
    value.to_string()
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
    let effort = state
        .thinking_effort
        .as_deref()
        .map(safe_line)
        .unwrap_or_else(|| "—".into());
    let effort_width = "Effort ".width() + effort.width();
    let metadata = crop_display(
        &format!(
            " {}  ·  {} / {}",
            safe_line(&state.workspace),
            safe_line(&state.profile),
            safe_line(&state.model)
        ),
        0,
        usize::from(area.width).saturating_sub(effort_width + 1),
    );
    let padding = usize::from(area.width).saturating_sub(metadata.width() + effort_width);
    lines.push(Line::from(vec![
        Span::styled(metadata, Style::default().fg(MUTED)),
        Span::raw(" ".repeat(padding)),
        Span::styled("Effort ", Style::default().fg(MUTED)),
        Span::styled(effort, Style::default().fg(EFFORT_COLOR)),
    ]));
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

fn draw_transcript(frame: &mut Frame<'_>, state: &mut State, area: Rect, framed: bool) {
    let block = if framed {
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(65, 73, 92)))
            .title(" Conversation ")
    } else {
        Block::default()
    };
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

fn draw_details(
    frame: &mut Frame<'_>,
    details: &Details,
    scroll: &mut usize,
    area: Rect,
    framed: bool,
) {
    let title = safe_line(&details.title[..prefix_boundary(&details.title, 256)]);
    let mut block = Block::default()
        .borders(if framed { Borders::ALL } else { Borders::TOP })
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
    let cached_truncated = details
        .lines
        .last()
        .is_some_and(|line| line == DETAIL_TRUNCATION_NOTICE);
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
                Style::default().fg(if line == DETAIL_TRUNCATION_NOTICE {
                    Color::Yellow
                } else {
                    FOREGROUND
                }),
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
    if (truncated || cached_truncated) && !title.starts_with(TRUNCATED_DETAIL_PREFIX) {
        block = block.title(format!(" {TRUNCATED_DETAIL_PREFIX}{title} "));
    }
    if truncated {
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

fn draw_editor(frame: &mut Frame<'_>, state: &State, area: Rect, compact: bool) {
    let title = if state.editor.truncated {
        " Input · paste truncated at 64 KiB "
    } else {
        " Message "
    };
    let block = if compact {
        Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(COMPACT_BLUE))
            .style(Style::default().fg(FOREGROUND).bg(INPUT_BACKGROUND))
    } else {
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(ACCENT))
    };
    let mut inner = block.inner(area);
    frame.render_widget(block, area);
    if compact && inner.height > 0 {
        inner.x += 1;
        inner.width = inner.width.saturating_sub(1);
        let label = if state.editor.truncated {
            "Message · paste truncated at 64 KiB"
        } else {
            "Message"
        };
        frame.render_widget(
            Paragraph::new(label),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        inner.y += 1;
        inner.height = inner.height.saturating_sub(1 + u16::from(area.height > 2));
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let (cursor_row, cursor_column) = state.editor.cursor_position();
    let first_row = cursor_row.saturating_sub(usize::from(inner.height) - 1);
    let first_column = cursor_column.saturating_sub(usize::from(inner.width) - 1);
    let text = safe_text(state.editor.text());
    let ghost = state
        .completion
        .current()
        .filter(|_| {
            state.completion.visible() && state.editor.cursor() == state.editor.text().len()
        })
        .and_then(|suggestion| suggestion.text.strip_prefix(state.editor.text()))
        .map(safe_line);
    let lines = text
        .split('\n')
        .enumerate()
        .skip(first_row)
        .take(usize::from(inner.height))
        .map(|(row, line)| {
            let draft = crop_display(line, first_column, usize::from(inner.width));
            let remaining =
                usize::from(inner.width).saturating_sub(UnicodeWidthStr::width(draft.as_str()));
            let mut spans = vec![Span::raw(draft)];
            if row == cursor_row
                && let Some(ghost) = &ghost
            {
                spans.push(Span::styled(
                    crop_display(ghost, 0, remaining),
                    Style::default().fg(MUTED),
                ));
            }
            Line::from(spans)
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), inner);
    frame.set_cursor_position((
        inner.x + (cursor_column - first_column) as u16,
        inner.y + (cursor_row - first_row) as u16,
    ));
}

fn draw_completion(frame: &mut Frame<'_>, state: &State, area: Rect) {
    if !state.completion.visible() || area.height < 3 || area.width < 4 {
        return;
    }
    let items = &state.completion.items;
    let rows = MAX_COMPLETION_ROWS
        .min(items.len())
        .min(usize::from(area.height - 2));
    if rows == 0 {
        return;
    }
    let selected = state.completion.selected.min(items.len() - 1);
    let first = selected
        .saturating_sub(rows - 1)
        .min(items.len().saturating_sub(rows));
    let popup = Rect::new(
        area.x + 1,
        area.bottom() - rows as u16 - 2,
        (area.width - 2).min(80),
        rows as u16 + 2,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Command suggestions ")
        .title_bottom(" ↑↓ Tab/→ Esc ")
        .border_style(Style::default().fg(ACCENT))
        .style(Style::default().bg(BACKGROUND));
    let inner = block.inner(popup);
    let text_width = usize::from(inner.width.saturating_sub(2));
    let commands = items
        .iter()
        .skip(first)
        .take(rows)
        .map(|item| crop_display(&safe_line(item.text), 0, text_width))
        .collect::<Vec<_>>();
    let command_width = commands
        .iter()
        .map(|command| UnicodeWidthStr::width(command.as_str()))
        .max()
        .unwrap_or(0);
    let lines = items
        .iter()
        .skip(first)
        .zip(commands)
        .enumerate()
        .map(|(row, (item, command))| {
            let active = row + first == selected;
            let style = if active {
                Style::default()
                    .fg(BACKGROUND)
                    .bg(ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(FOREGROUND).bg(BACKGROUND)
            };
            let padding = command_width - UnicodeWidthStr::width(command.as_str()) + 2;
            let mut spans = vec![
                Span::raw(if active { "› " } else { "  " }),
                Span::raw(command),
            ];
            if text_width >= command_width + 6 {
                spans.push(Span::raw(" ".repeat(padding)));
                spans.push(Span::styled(
                    crop_display(
                        &safe_line(item.description),
                        0,
                        text_width - command_width - 2,
                    ),
                    Style::default().fg(if active { BACKGROUND } else { MUTED }),
                ));
            }
            Line::from(spans).style(style)
        })
        .collect::<Vec<_>>();
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    frame.render_widget(Paragraph::new(lines), inner);
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
    use crate::commands::Suggestion;
    use ratatui::{
        Terminal,
        backend::{Backend, TestBackend},
    };

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

    fn row_text(buffer: &ratatui::buffer::Buffer, row: u16) -> String {
        (0..buffer.area.width)
            .map(|column| buffer[(column, row)].symbol())
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
    fn details_setter_bounds_owned_bytes_rows_and_allocation_without_changing_scroll() {
        let mut state = State {
            details_scroll: 17,
            ..State::default()
        };
        state.set_details(Details {
            title: "Parameters".into(),
            lines: vec!["界".repeat(12 * 1024 * 1024)],
        });
        let details = state.details.as_ref().unwrap();
        assert_eq!(details.title, "Parameters");
        assert_eq!(details.lines.last().unwrap(), DETAIL_TRUNCATION_NOTICE);
        assert!(details.lines.iter().map(String::len).sum::<usize>() <= MAX_TRANSCRIPT_BYTES);
        assert!(details.lines.iter().map(String::capacity).sum::<usize>() <= MAX_TRANSCRIPT_BYTES);
        assert!(details.lines.len() <= MAX_DETAIL_LINES);
        assert_eq!(state.details_scroll, 17);
        assert!(render(&mut state, 60, 20).contains("Truncated"));
        state.set_details(Details {
            title: "Rows".into(),
            lines: vec!["line\n".repeat(MAX_DETAIL_LINES + 20)],
        });
        let details = state.details.as_ref().unwrap();
        assert_eq!(details.lines.len(), MAX_DETAIL_LINES);
        assert_eq!(details.lines.last().unwrap(), DETAIL_TRUNCATION_NOTICE);
        assert!(details.lines.capacity() <= MAX_DETAIL_LINES);
    }

    #[test]
    fn draw_normalizes_direct_details_even_on_tiny_frames_and_is_idempotent() {
        let mut state = State {
            details: Some(Details {
                title: "Direct assignment".into(),
                lines: vec!["x".repeat(MAX_TRANSCRIPT_BYTES + 1)],
            }),
            ..State::default()
        };
        render(&mut state, 1, 1);
        let details = state.details.as_ref().unwrap();
        assert!(details.lines.iter().map(String::capacity).sum::<usize>() <= MAX_TRANSCRIPT_BYTES);
        let title = details.title.clone();
        let lines = details.lines.clone();
        render(&mut state, 60, 20);
        assert_eq!(state.details.as_ref().unwrap().title, title);
        assert_eq!(state.details.as_ref().unwrap().lines, lines);
    }

    #[test]
    fn unused_details_capacity_is_released_without_claiming_data_truncation() {
        let mut source = String::with_capacity(MAX_TRANSCRIPT_BYTES * 4);
        source.push_str("retained");
        let mut state = State::default();
        state.set_details(Details {
            title: "Tool".into(),
            lines: vec![source],
        });
        let details = state.details.as_ref().unwrap();
        assert_eq!(details.title, "Tool");
        assert_eq!(details.lines, ["retained"]);
        assert_eq!(details.lines[0].capacity(), "retained".len());
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

    #[test]
    fn completion_ghost_keeps_the_draft_cursor_and_details_unchanged() {
        let mut state = State::default();
        state.editor.set_text("/per");
        state
            .completion
            .update(state.editor.text(), state.editor.cursor(), true);
        state.set_details(Details {
            title: "Saved records".into(),
            lines: vec!["state unknown".into()],
        });
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        assert_eq!((cursor.x, cursor.y), (5, 21));
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(cursor.x, cursor.y)].symbol(), "m");
        assert_eq!(buffer[(cursor.x, cursor.y)].fg, MUTED);
        assert_eq!(state.editor.text(), "/per");
        assert_eq!(state.editor.cursor(), 4);
        assert_eq!(state.details.as_ref().unwrap().title, "Saved records");
        assert_eq!(state.details.as_ref().unwrap().lines, ["state unknown"]);
        assert!(
            buffer
                .content
                .iter()
                .any(|cell| { cell.symbol() == "›" && cell.bg == ACCENT })
        );
        let image = render(&mut state, 120, 24);
        assert!(image.contains("Command suggestions"));
        assert!(image.contains("Tab/→ complete"));
    }

    #[test]
    fn dismissed_completion_clears_its_popup_and_ghost_on_the_same_terminal() {
        let mut state = State::default();
        state.editor.set_text("/per");
        state
            .completion
            .update(state.editor.text(), state.editor.cursor(), true);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        state.completion.dismiss();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let image = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!image.contains("Command suggestions"));
        assert!(!image.contains('›'));
        assert!(!image.contains("/permissions"));
        assert_eq!(terminal.backend().buffer()[(7, 21)].symbol(), " ");
        assert_eq!(state.editor.text(), "/per");
    }

    #[test]
    fn narrow_completion_keeps_the_selected_row_in_its_six_row_window() {
        let mut state = State::default();
        state.editor.set_text("/");
        state.completion.update("/", 1, true);
        assert!(state.completion.items.len() > MAX_COMPLETION_ROWS);
        state.completion.selected = state.completion.items.len() - 1;
        let selected = state.completion.current().unwrap().text;
        let mut terminal = Terminal::new(TestBackend::new(26, 16)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        let row = (0..16)
            .find(|row| buffer[(3, *row)].symbol() == "›")
            .unwrap();
        assert_eq!(row, 9);
        assert_eq!(buffer[(3, row)].bg, ACCENT);
        let actual = (5..23)
            .map(|column| buffer[(column, row)].symbol())
            .collect::<String>();
        assert_eq!(actual.trim_end(), crop_display(selected, 0, 18));
        assert_eq!(
            buffer
                .content
                .iter()
                .filter(|cell| cell.symbol() == "›")
                .count(),
            1
        );
        assert_eq!(state.editor.text(), "/");
    }

    #[test]
    fn ghost_respects_unicode_horizontal_cropping_and_end_of_draft() {
        let mut state = State::default();
        state.editor.set_text("/界界界");
        state.completion.update("/", 1, true);
        state.completion.items = vec![Suggestion {
            text: "/界界界ab",
            description: "Unicode preview",
        }];
        let mut terminal = Terminal::new(TestBackend::new(6, 3)).unwrap();
        terminal
            .draw(|frame| draw_editor(frame, &state, frame.area(), false))
            .unwrap();
        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        assert_eq!((cursor.x, cursor.y), (4, 1));
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(1, 1)].symbol(), " ");
        assert_eq!(buffer[(2, 1)].symbol(), "界");
        assert_eq!(buffer[(4, 1)].symbol(), "a");
        assert_eq!(buffer[(4, 1)].fg, MUTED);
        assert_eq!(state.editor.text(), "/界界界");
        state.editor.move_left();
        terminal
            .draw(|frame| draw_editor(frame, &state, frame.area(), false))
            .unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .all(|cell| { cell.symbol() != "a" && cell.symbol() != "b" })
        );
    }

    #[test]
    fn completion_degrades_safely_on_small_frames_with_bounded_history() {
        let mut state = State::default();
        for _ in 0..12 {
            state.push_message(Role::Tool, "Tool", "界".repeat(MAX_MESSAGE_BYTES / 3));
        }
        state.editor.set_text("/per");
        state
            .completion
            .update(state.editor.text(), state.editor.cursor(), true);
        for (width, height) in [(0, 0), (1, 1), (19, 5), (20, 6), (30, 10), (120, 30)] {
            render(&mut state, width, height);
            assert_eq!(state.editor.text(), "/per");
        }
        let image = render(&mut state, 20, 6);
        assert!(image.contains("/permissions"));
        assert!(!image.contains('›'));
        assert!(state.dropped_messages > 0);
        assert!(state.messages.len() <= MAX_MESSAGES);
    }

    #[test]
    fn compact_layout_keeps_status_input_and_footer_in_their_observable_regions() {
        let mut state = State {
            workspace: "/tmp/project".into(),
            model: "configured-model".into(),
            context_window_tokens: Some(128_000),
            context_input_tokens: Some(64_000),
            ..State::default()
        };
        state.editor.set_text("hello");
        for (width, height, status_row, title_row, input_row) in
            [(80, 24, 19, 20, 21), (60, 18, 13, 14, 15), (20, 6, 2, 3, 4)]
        {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let cursor = terminal.backend_mut().get_cursor_position().unwrap();
            assert_eq!((cursor.x, cursor.y), (8, input_row));
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(0, 0)].bg, COMPACT_BACKGROUND);
            assert!(row_text(buffer, 0).contains("Saya Harness"));
            assert!(row_text(buffer, 0).contains("Ctrl+P"));
            assert!(row_text(buffer, 1).contains("/tmp/project"));
            assert!(row_text(buffer, status_row).contains("✓ Ready"));
            assert!(row_text(buffer, status_row).ends_with("auto│"));
            assert_eq!(buffer[(3, title_row)].symbol(), "M");
            assert_eq!(buffer[(1, title_row)].symbol(), "│");
            assert_eq!(buffer[(1, title_row)].fg, COMPACT_BLUE);
            assert_eq!(buffer[(2, title_row)].bg, INPUT_BACKGROUND);
            assert_eq!(buffer[(3, input_row)].symbol(), "h");
            assert_eq!(buffer[(3, input_row)].bg, INPUT_BACKGROUND);
            let footer = row_text(buffer, height - 1);
            assert!(footer.starts_with("└WORK"));
            assert!(footer.contains("input"));
            assert!(footer.contains("50%"));
            assert!(footer.contains('◑'));
            if width >= 44 {
                assert!(row_text(buffer, 0).contains(env!("CARGO_PKG_VERSION")));
                assert!(footer.contains("64k/128k"));
            }
            assert_eq!(state.editor.text(), "hello");
        }
    }

    #[test]
    fn compact_breakpoints_preserve_the_large_terminal_header_and_usage() {
        let mut state = State {
            usage: "wide-usage-marker".into(),
            ..State::default()
        };
        for (width, height) in [(100, 24), (120, 30)] {
            let image = render(&mut state, width, height);
            assert!(image.contains("SAYA"));
            assert!(image.contains("wide-usage-marker"));
            assert!(image.contains("Conversation"));
            assert!(!image.contains("Saya Harness"));
        }
        for (width, height) in [(99, 24), (100, 23)] {
            let image = render(&mut state, width, height);
            assert!(image.contains("Saya Harness"));
            assert!(!image.contains("wide-usage-marker"));
            assert!(!image.contains("Conversation"));
        }
    }

    #[test]
    fn input_meter_distinguishes_unknown_estimated_and_reported_input_without_overflow() {
        let mut state = State::default();
        assert_eq!(state.session_title, "New session");
        assert_eq!(context_meter(&state, false), "○ input —/— (—)");
        state.context_window_tokens = Some(128_000);
        assert_eq!(context_meter(&state, false), "○ input —/128k (—)");
        state.context_input_tokens = Some(64_000);
        state.context_estimated = true;
        assert_eq!(context_meter(&state, false), "◑ input ~64k/128k (50%)");
        assert_eq!(context_meter(&state, true), "◑ input ~50%");
        state.context_estimated = false;
        state.context_window_tokens = Some(100);
        for (input, ring) in [(0, '○'), (25, '◔'), (50, '◑'), (75, '◕'), (100, '●')] {
            state.context_input_tokens = Some(input);
            assert!(context_meter(&state, false).starts_with(ring));
        }
        state.context_input_tokens = Some(u64::MAX);
        state.context_window_tokens = Some(1);
        assert!(context_meter(&state, false).ends_with("(>100%)"));
        assert_eq!(context_meter(&state, true), "● input >100%");
        state.context_window_tokens = Some(0);
        assert!(context_meter(&state, false).ends_with("/— (—)"));
        assert_eq!(context_meter(&state, true), "○ input —");
    }

    #[test]
    fn compact_multiline_editor_scrolls_within_its_gray_input_and_keeps_safety_markers() {
        let mut state = State {
            status: "Awaiting approval".into(),
            permissions: "Manual Approval".into(),
            workspace: "project\x1b[2J\u{202e}".into(),
            thinking_effort: Some("high".into()),
            ..State::default()
        };
        state
            .editor
            .set_text("one\ntwo\nthree\nfour\nfive\ne\u{301}界👩‍💻");
        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        assert_eq!(cursor.y, 15);
        assert!(cursor.x < 59);
        let buffer = terminal.backend().buffer();
        assert!(row_text(buffer, 11).contains("Awaiting approval"));
        assert!(row_text(buffer, 11).ends_with("Manual Approval│"));
        assert_eq!(buffer[(3, 12)].symbol(), "M");
        assert!(row_text(buffer, 13).contains("four"));
        assert!(row_text(buffer, 14).contains("five"));
        assert!(row_text(buffer, 15).contains("e\u{301}界"));
        assert_eq!(buffer[(3, 16)].bg, INPUT_BACKGROUND);
        assert!(row_text(buffer, 17).contains("high"));
        let image = render(&mut state, 60, 18);
        assert!(image.contains("\\u{1b}"));
        assert!(!image.contains('\x1b'));
        assert!(!image.contains('\u{202e}'));
        state.editor.clear();
        let tiny = render(&mut state, 20, 6);
        assert!(tiny.contains("Approval"));
        assert!(tiny.contains("manual"));
    }

    #[test]
    fn selected_effort_remains_visible_when_model_or_workspace_names_are_long() {
        let mut state = State {
            workspace: "/long-workspace".repeat(12),
            model: "long-model-name".repeat(12),
            thinking_effort: Some("max".into()),
            context_window_tokens: Some(128_000),
            context_input_tokens: Some(64_000),
            ..State::default()
        };
        for (width, height) in [(80, 24), (60, 18)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let buffer = terminal.backend().buffer();
            let footer = row_text(buffer, height - 1);
            assert!(footer.contains(" max"));
            assert!(footer.contains("input 64k/128k (50%)"));
            assert!(
                buffer
                    .content
                    .iter()
                    .any(|cell| cell.symbol() == "m" && cell.fg == EFFORT_COLOR)
            );
        }
        let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert!(row_text(terminal.backend().buffer(), 1).ends_with("Effort max"));
        state.thinking_effort = None;
        assert!(render(&mut state, 110, 32).contains("Effort —"));
    }
}
