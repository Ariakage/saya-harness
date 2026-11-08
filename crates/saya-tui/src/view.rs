//! Pure, bounded TUI state and rendering. Runtime effects belong to the controller.
use crate::{
    commands::{COMMANDS, Completion},
    editor::{Editor, prefix_boundary, safe_text},
    rich_text::{append_wrapped_line, markdown_lines},
    theme::{
        AMBER, BACKGROUND, BLUE, BORDER, CODE_BACKGROUND, CODE_FOREGROUND, FOREGROUND, GREEN,
        INPUT_BACKGROUND, INPUT_BORDER, MUTED, RED, SIDEBAR_BACKGROUND, SOFT_AMBER,
        STRONG_FOREGROUND, mode_color,
    },
    tool_summary::summarize,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use saya_core::tools::todo::{TodoItem, TodoStatus};
use std::collections::{HashSet, VecDeque};
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
const MAX_COMPLETION_ROWS: usize = 6;
const MAX_CONTEXT_ENTRIES: usize = 9;
const TOOL_COLLAPSE_BYTES: usize = 512;
const TOOL_COLLAPSE_LINES: usize = 6;
const MAX_TODOS: usize = 64;
const WHEEL_LINES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Tool,
    Thinking,
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

#[derive(Debug, Clone)]
pub struct ContextEntry {
    pub label: String,
    pub tokens: u64,
}

#[derive(Debug)]
struct MessageHit {
    area: Rect,
    id: u64,
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
    pub context_breakdown: Vec<ContextEntry>,
    pub context_breakdown_estimated: bool,
    pub skills_count: Option<usize>,
    pub thinking_effort: Option<String>,
    pub todos: Vec<TodoItem>,
    pub editor: Editor,
    pub completion: Completion,
    /// Selected catalog index while the command modal is open.
    pub command_palette: Option<usize>,
    pub messages: VecDeque<Message>,
    pub details: Option<Details>,
    /// Visual line offset from the top of the current details pane.
    pub details_scroll: usize,
    /// Page movement in the last rendered details viewport.
    pub details_page_rows: usize,
    /// Visual lines above the live bottom. Zero follows streamed output.
    pub scroll: usize,
    pub dropped_messages: usize,
    next_id: u64,
    last_width: u16,
    last_line_count: usize,
    expanded_messages: HashSet<u64>,
    message_hits: Vec<MessageHit>,
    transcript_viewport: Option<Rect>,
    details_viewport: Option<Rect>,
    completion_viewport: Option<Rect>,
    palette_hits: Vec<(Rect, usize)>,
    palette_first: usize,
    fold_anchor: Option<(u64, u16)>,
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
            context_breakdown: Vec::new(),
            context_breakdown_estimated: true,
            skills_count: None,
            thinking_effort: None,
            todos: Vec::new(),
            editor: Editor::default(),
            completion: Completion::default(),
            command_palette: None,
            messages: VecDeque::new(),
            details: None,
            details_scroll: 0,
            details_page_rows: 10,
            scroll: 0,
            dropped_messages: 0,
            next_id: 1,
            last_width: 0,
            last_line_count: 0,
            expanded_messages: HashSet::new(),
            message_hits: Vec::new(),
            transcript_viewport: None,
            details_viewport: None,
            completion_viewport: None,
            palette_hits: Vec::new(),
            palette_first: 0,
            fold_anchor: None,
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
        self.todos.clear();
        self.scroll = 0;
        self.details_scroll = 0;
        self.details_page_rows = 10;
        self.dropped_messages = 0;
        self.last_width = 0;
        self.last_line_count = 0;
        self.expanded_messages.clear();
        self.message_hits.clear();
        self.transcript_viewport = None;
        self.details_viewport = None;
        self.completion_viewport = None;
        self.command_palette = None;
        self.palette_hits.clear();
        self.palette_first = 0;
        self.fold_anchor = None;
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
        self.fold_anchor = None;
        self.scroll = self.scroll.saturating_add(lines);
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.fold_anchor = None;
        self.scroll = self.scroll.saturating_sub(lines);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.fold_anchor = None;
        self.scroll = 0;
    }

    /// Toggle a foldable message only when its actual visible header was clicked.
    /// Coordinates are terminal cells from the most recent render.
    pub fn toggle_at(&mut self, x: u16, y: u16) -> bool {
        if self.command_palette.is_some() || contains(self.completion_viewport, x, y) {
            return false;
        }
        let Some(hit) = self
            .message_hits
            .iter()
            .find(|hit| hit.area.contains((x, y).into()))
        else {
            return false;
        };
        let id = hit.id;
        self.toggle_message(id, hit.area.y)
    }

    /// Toggle the latest retained thinking or long tool message.
    /// This supplies a keyboard fallback for mouse interaction.
    pub fn toggle_latest(&mut self) -> bool {
        if self.command_palette.is_some() {
            return false;
        }
        let Some(message) = self.messages.iter().rev().find(|message| foldable(message)) else {
            return false;
        };
        let id = message.id;
        let row = self
            .message_hits
            .iter()
            .find(|hit| hit.id == id)
            .map_or_else(
                || self.transcript_viewport.map_or(0, |area| area.y),
                |hit| hit.area.y,
            );
        self.toggle_message(id, row)
    }

    fn toggle_message(&mut self, id: u64, row: u16) -> bool {
        if !self
            .messages
            .iter()
            .any(|message| message.id == id && foldable(message))
        {
            return false;
        }
        if !self.expanded_messages.remove(&id) {
            self.expanded_messages.insert(id);
        }
        self.fold_anchor = Some((id, row));
        true
    }

    /// Scroll the rendered transcript or details viewport by three visual rows.
    /// Editor, sidebar, popup and terminal chrome have no scroll side effects.
    pub fn scroll_at(&mut self, x: u16, y: u16, up: bool) -> bool {
        if self.command_palette.is_some() || contains(self.completion_viewport, x, y) {
            return false;
        }
        if contains(self.details_viewport, x, y) {
            self.details_scroll = if up {
                self.details_scroll.saturating_sub(WHEEL_LINES)
            } else {
                self.details_scroll.saturating_add(WHEEL_LINES)
            };
            return true;
        }
        if contains(self.transcript_viewport, x, y) {
            if up {
                self.scroll_up(WHEEL_LINES);
            } else {
                self.scroll_down(WHEEL_LINES);
            }
            return true;
        }
        false
    }

    /// Select only a candidate row drawn in the latest command modal frame.
    pub fn palette_index_at(&self, x: u16, y: u16) -> Option<usize> {
        self.command_palette?;
        self.palette_hits
            .iter()
            .find(|(area, _)| area.contains((x, y).into()))
            .map(|(_, index)| *index)
    }

    /// Move the modal selection by one entry, clamping at the catalog ends.
    /// Returns whether the open modal consumed the navigation event.
    pub fn palette_scroll(&mut self, up: bool) -> bool {
        let Some(selected) = &mut self.command_palette else {
            return false;
        };
        let last = COMMANDS.len().saturating_sub(1);
        *selected = (*selected).min(last);
        *selected = if up {
            selected.saturating_sub(1)
        } else {
            selected.saturating_add(1).min(last)
        };
        true
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
        self.expanded_messages
            .retain(|id| self.messages.iter().any(|message| message.id == *id));
        self.todos.truncate(MAX_TODOS);
    }
}

fn contains(area: Option<Rect>, x: u16, y: u16) -> bool {
    area.is_some_and(|area| area.contains((x, y).into()))
}

fn foldable(message: &Message) -> bool {
    message.role == Role::Thinking
        || (message.role == Role::Tool
            && (message.text.len() > TOOL_COLLAPSE_BYTES
                || message
                    .text
                    .split('\n')
                    .take(TOOL_COLLAPSE_LINES + 1)
                    .count()
                    > TOOL_COLLAPSE_LINES
                || summarize(&message.text).is_some()))
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
    state.message_hits.clear();
    state.transcript_viewport = None;
    state.details_viewport = None;
    state.completion_viewport = None;
    state.palette_hits.clear();
    if let Some(selected) = &mut state.command_palette {
        *selected = (*selected).min(COMMANDS.len().saturating_sub(1));
    }
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
    if area.width >= 120 && area.height >= 24 {
        let sidebar_width = (area.width / 4).clamp(28, 40);
        let main_width = area.width - sidebar_width;
        draw_session(
            frame,
            state,
            Rect::new(area.x, area.y, main_width, area.height),
        );
        draw_sidebar(
            frame,
            state,
            Rect::new(area.x + main_width, area.y, sidebar_width, area.height),
        );
    } else {
        draw_session(frame, state, area);
    }
    draw_command_palette(frame, state);
}

fn draw_body(frame: &mut Frame<'_>, state: &mut State, body: Rect) {
    if state.details.is_some() && body.width >= 98 {
        let columns = Layout::horizontal([
            Constraint::Min(50),
            Constraint::Length((body.width / 3).clamp(32, 48)),
        ])
        .split(body);
        draw_transcript(frame, state, columns[0]);
        if let Some(details) = &state.details {
            state.details_viewport = Some(columns[1]);
            draw_details(
                frame,
                details,
                &mut state.details_scroll,
                &mut state.details_page_rows,
                columns[1],
            );
        }
    } else if let Some(details) = &state.details {
        state.details_viewport = Some(body);
        draw_details(
            frame,
            details,
            &mut state.details_scroll,
            &mut state.details_page_rows,
            body,
        );
    } else {
        state.details_page_rows = 10;
        draw_transcript(frame, state, body);
    }
}

fn draw_session(frame: &mut Frame<'_>, state: &mut State, area: Rect) {
    let brand = Style::default()
        .fg(STRONG_FOREGROUND)
        .add_modifier(Modifier::BOLD);
    let title = if area.width < 44 {
        Line::styled("Saya Harness", brand)
    } else {
        Line::from(vec![
            Span::styled("Saya Harness ", brand),
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
    } else if state.editor.text().is_empty() && state.command_palette.is_none() {
        if area.width >= 70 {
            "Tab mode · Ctrl+P commands"
        } else if area.width >= 44 {
            "Tab mode · Ctrl+P"
        } else {
            "Ctrl+P"
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
        .border_style(Style::default().fg(BORDER))
        .title(Line::styled(
            format!(" • {} ", safe_line(&state.workspace)),
            Style::default().fg(MUTED),
        ));
    let inner = block.inner(outer);
    frame.render_widget(block, outer);
    let input_lines = state
        .editor
        .text()
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    let input_height = (input_lines + 3).clamp(4, 6).min(usize::from(
        inner
            .height
            .saturating_sub(if state.details.is_some() { 2 } else { 1 }),
    )) as u16;
    let rows = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(input_height),
    ])
    .split(inner);
    draw_body(frame, state, rows[0]);
    draw_status(frame, state, rows[1]);
    draw_editor(frame, state, rows[2]);
    draw_completion(frame, state, rows[0]);
    draw_footer(
        frame,
        state,
        Rect::new(inner.x, outer.bottom() - 1, inner.width, 1),
    );
}

fn draw_sidebar(frame: &mut Frame<'_>, state: &State, area: Rect) {
    frame.render_widget(
        Block::default().style(Style::default().fg(FOREGROUND).bg(SIDEBAR_BACKGROUND)),
        area,
    );
    let content = Rect::new(
        area.x + 1,
        area.y,
        area.width.saturating_sub(2),
        area.height,
    );
    let heading = Style::default()
        .fg(STRONG_FOREGROUND)
        .add_modifier(Modifier::BOLD);
    let compact_tasks = area.height < 32 && !state.todos.is_empty();
    let context_limit = if compact_tasks {
        6
    } else {
        MAX_CONTEXT_ENTRIES
    };
    let total_row = if compact_tasks { 10 } else { 13 };
    let extensions_row = if compact_tasks {
        usize::from(area.height) - 5
    } else {
        15
    };
    let tasks_row = if compact_tasks { 12 } else { 20 };
    let mut lines = vec![Line::default(); usize::from(area.height.min(40))];
    for row in [1, extensions_row - 1, tasks_row - 1] {
        lines[row] = Line::styled(
            "─".repeat(usize::from(content.width)),
            Style::default().fg(BORDER),
        );
    }
    lines[0] = Line::styled(
        safe_line(&state.session_title[..prefix_boundary(&state.session_title, 256)]),
        heading,
    );
    lines[2] = Line::from(vec![
        Span::styled("Context", heading),
        Span::styled(
            if state.context_breakdown.is_empty() {
                " · Unknown"
            } else if state.context_breakdown_estimated {
                " · Estimated"
            } else {
                ""
            },
            Style::default().fg(MUTED),
        ),
    ]);
    lines[3] = Line::styled(
        format!(
            "Window {}",
            state
                .context_window_tokens
                .filter(|tokens| *tokens > 0)
                .map(token_label)
                .unwrap_or_else(|| "—".into())
        ),
        Style::default().fg(MUTED),
    );
    if state.context_breakdown.is_empty() {
        lines[4] = Line::styled("Unknown before request", Style::default().fg(MUTED));
    } else {
        for (index, line) in context_rows(state, usize::from(content.width))
            .into_iter()
            .take(context_limit)
            .enumerate()
        {
            lines[index + 4] = line;
        }
        if state.context_breakdown.len() > context_limit {
            lines[total_row] =
                Line::styled("Additional entries omitted", Style::default().fg(MUTED));
        } else {
            let total = state
                .context_breakdown
                .iter()
                .map(|entry| u128::from(entry.tokens))
                .sum::<u128>();
            let summary = match u64::try_from(total) {
                Ok(total) => format!(
                    "Total {}{} {}",
                    if state.context_breakdown_estimated {
                        "~"
                    } else {
                        ""
                    },
                    token_label(total),
                    sidebar_percentage(total, state.context_window_tokens)
                ),
                Err(_) => "Total estimate exceeds range".into(),
            };
            lines[total_row] = Line::styled(summary, Style::default().fg(MUTED));
        }
    }
    lines[extensions_row] = Line::styled("Extensions", heading);
    lines[extensions_row + 1] = Line::styled("MCP  Not implemented", Style::default().fg(MUTED));
    lines[extensions_row + 2] =
        Line::styled("Plugins  Not implemented", Style::default().fg(MUTED));
    lines[extensions_row + 3] = Line::styled(
        match state.skills_count {
            Some(count) => format!("Skills {count} discovered"),
            None => "Skills —".into(),
        },
        Style::default().fg(MUTED),
    );
    let completed = state
        .todos
        .iter()
        .filter(|todo| todo.status == TodoStatus::Completed)
        .count();
    lines[tasks_row] = Line::from(vec![
        Span::styled("Tasks", heading),
        Span::styled(
            format!(" · {completed}/{}", state.todos.len()),
            Style::default().fg(MUTED),
        ),
    ]);
    let task_capacity = if compact_tasks {
        extensions_row.saturating_sub(tasks_row + 2)
    } else {
        usize::from(area.height)
            .saturating_sub(tasks_row + 3)
            .min(9)
    };
    let mut task_end = tasks_row + 1;
    if state.todos.is_empty() {
        lines[task_end] = Line::styled("No tasks", Style::default().fg(MUTED));
        task_end += 1;
    } else {
        let visible = state
            .todos
            .len()
            .min(task_capacity.saturating_sub(usize::from(state.todos.len() > task_capacity)));
        for todo in state.todos.iter().take(visible) {
            let (icon, color) = match todo.status {
                TodoStatus::Pending => ("○", MUTED),
                TodoStatus::InProgress => ("◐", BLUE),
                TodoStatus::Completed => ("✓", GREEN),
            };
            let text = safe_line(&todo.content[..prefix_boundary(&todo.content, 1024)]);
            let text_width = usize::from(content.width.saturating_sub(2));
            let cropped = crop_display(&text, 0, text_width);
            let text = if cropped.len() < text.len() && text_width > 0 {
                format!("{}…", crop_display(&text, 0, text_width - 1))
            } else {
                cropped
            };
            lines[task_end] = Line::from(vec![
                Span::styled(format!("{icon} "), Style::default().fg(color)),
                Span::styled(
                    text,
                    match todo.status {
                        TodoStatus::Completed => Style::default().fg(MUTED),
                        TodoStatus::InProgress => Style::default()
                            .fg(STRONG_FOREGROUND)
                            .add_modifier(Modifier::BOLD),
                        TodoStatus::Pending => Style::default().fg(FOREGROUND),
                    },
                ),
            ]);
            task_end += 1;
        }
        if visible < state.todos.len() {
            lines[task_end] = Line::styled(
                format!("+{} more tasks", state.todos.len() - visible),
                Style::default().fg(MUTED),
            );
            task_end += 1;
        }
    }
    let details_row = task_end + 1;
    if !compact_tasks && details_row + 1 < usize::from(area.height.saturating_sub(1)) {
        lines[details_row - 1] = Line::styled(
            "─".repeat(usize::from(content.width)),
            Style::default().fg(BORDER),
        );
        lines[details_row] = Line::styled("Details", heading);
    }
    for (index, (label, value)) in [
        ("Profile", state.profile.as_str()),
        ("Model", state.model.as_str()),
        ("Mode", state.mode.as_str()),
        ("Permissions", state.permissions.as_str()),
        ("Effort", state.thinking_effort.as_deref().unwrap_or("—")),
        ("Status", state.status.as_str()),
    ]
    .into_iter()
    .enumerate()
    {
        let row = index + details_row + 1;
        if !compact_tasks && row < usize::from(area.height.saturating_sub(2)).min(lines.len()) {
            lines[row] = Line::styled(
                format!("{label} {}", safe_line(value)),
                Style::default().fg(MUTED),
            );
        }
    }
    frame.render_widget(Paragraph::new(lines), content);
    frame.render_widget(
        Paragraph::new(safe_line(&state.workspace)).style(Style::default().fg(MUTED)),
        Rect::new(content.x, area.bottom() - 1, content.width, 1),
    );
}

fn context_rows(state: &State, width: usize) -> Vec<Line<'static>> {
    let entries = state
        .context_breakdown
        .iter()
        .take(MAX_CONTEXT_ENTRIES)
        .map(|entry| {
            let metric = format!(
                "{} {}",
                token_label(entry.tokens),
                sidebar_percentage(entry.tokens, state.context_window_tokens)
            );
            (entry, metric)
        })
        .collect::<Vec<_>>();
    let metric_width = entries
        .iter()
        .map(|(_, metric)| metric.width())
        .max()
        .unwrap_or(0);
    let label_width = 12.min(width.saturating_sub(metric_width + 5));
    let bar_width = width.saturating_sub(label_width + metric_width + 2).min(8);
    entries
        .into_iter()
        .map(|(entry, metric)| {
            let label = crop_display(
                &safe_line(&entry.label[..prefix_boundary(&entry.label, 256)]),
                0,
                label_width,
            );
            let (filled, remaining) =
                context_bar(entry.tokens, state.context_window_tokens, bar_width);
            let padding = width.saturating_sub(label_width + bar_width + metric.width() + 2);
            Line::from(vec![
                Span::styled(
                    format!(
                        "{label}{} ",
                        " ".repeat(label_width.saturating_sub(label.width()))
                    ),
                    Style::default().fg(MUTED),
                ),
                Span::styled(filled, Style::default().fg(BLUE)),
                Span::styled(remaining, Style::default().fg(BORDER)),
                Span::styled(
                    format!(" {}{metric}", " ".repeat(padding)),
                    Style::default().fg(if entry.tokens == 0 {
                        MUTED
                    } else {
                        STRONG_FOREGROUND
                    }),
                ),
            ])
        })
        .collect()
}

fn sidebar_percentage(input: u64, window: Option<u64>) -> String {
    let Some(window) = window.filter(|window| *window > 0) else {
        return "—".into();
    };
    if input > window {
        return ">100%".into();
    }
    let tenths = u128::from(input) * 1000 / u128::from(window);
    if input > 0 && tenths == 0 {
        "<0.1%".into()
    } else if tenths % 10 == 0 {
        format!("{}%", tenths / 10)
    } else {
        format!("{}.{}%", tenths / 10, tenths % 10)
    }
}

fn context_bar(input: u64, window: Option<u64>, width: usize) -> (String, String) {
    let Some(window) = window.filter(|window| *window > 0) else {
        return (String::new(), "─".repeat(width));
    };
    if width == 0 {
        return (String::new(), String::new());
    }
    let eighths =
        (u128::from(input) * width as u128 * 8 / u128::from(window)).min(width as u128 * 8);
    let eighths = if input > 0 { eighths.max(1) } else { eighths } as usize;
    let full = eighths / 8;
    let fraction = eighths % 8;
    let filled = format!(
        "{}{}",
        "█".repeat(full),
        ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"][fraction]
    );
    let remaining = width.saturating_sub(full + usize::from(fraction > 0));
    (filled, "░".repeat(remaining))
}

fn context_percentage(input: u64, window: Option<u64>) -> Option<u64> {
    window.filter(|window| *window > 0).map(|window| {
        if input > window {
            101
        } else {
            (u128::from(input) * 100 / u128::from(window)) as u64
        }
    })
}

fn percentage_label(percent: Option<u64>) -> String {
    match percent {
        Some(101) => ">100%".into(),
        Some(value) => format!("{value}%"),
        None => "—".into(),
    }
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

fn draw_status(frame: &mut Frame<'_>, state: &State, area: Rect) {
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
    let status_color = match state.status.as_str() {
        "Ready" | "Completed" => GREEN,
        "Failed" => RED,
        "Awaiting approval" | "Awaiting input" | "NeedsConfirmation" | "NeedsInput"
        | "LimitReached" => AMBER,
        "Running" | "Reviewing" | "Thinking" | "Responding" => BLUE,
        _ => MUTED,
    };
    draw_split_line(
        frame,
        Line::from(vec![
            Span::styled(
                if ready { "✓ " } else { "● " },
                Style::default().fg(status_color),
            ),
            Span::styled(safe_line(status), Style::default().fg(MUTED)),
        ]),
        Line::styled(
            safe_line(permissions),
            Style::default().fg(if state.permissions == "auto" {
                MUTED
            } else {
                SOFT_AMBER
            }),
        ),
        area,
        6,
    );
}

fn draw_footer(frame: &mut Frame<'_>, state: &State, area: Rect) {
    let mut mode = Line::from(Span::styled(
        safe_line(&state.mode),
        Style::default().fg(mode_color(&state.mode)),
    ));
    if let Some(pending) = &state.pending_mode {
        mode.spans
            .push(Span::styled("→", Style::default().fg(MUTED)));
        mode.spans.push(Span::styled(
            safe_line(pending),
            Style::default().fg(mode_color(pending)),
        ));
    }
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
    let mut left = mode;
    left.spans.push(Span::styled(
        format!(
            " {}",
            crop_display(&safe_line(&state.model), 0, model_width)
        ),
        Style::default().fg(FOREGROUND),
    ));
    if show_effort {
        left.spans.push(Span::styled(
            format!(" {effort}"),
            Style::default().fg(if state.thinking_effort.is_some() {
                AMBER
            } else {
                MUTED
            }),
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
        .and_then(|input| context_percentage(input, window));
    let ring = match percent {
        Some(0) | None => "○",
        Some(1..=25) => "◔",
        Some(26..=50) => "◑",
        Some(51..=75) => "◕",
        Some(_) => "●",
    };
    let percentage = percentage_label(percent);
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

fn safe_line(text: &str) -> String {
    safe_text(text).replace('\n', "\\n")
}

fn role_style(role: Role) -> Style {
    Style::default().fg(match role {
        Role::User => BLUE,
        Role::Assistant => FOREGROUND,
        Role::Tool => AMBER,
        Role::Thinking => MUTED,
        Role::System => MUTED,
    })
}

fn draw_transcript(frame: &mut Frame<'_>, state: &mut State, mut inner: Rect) {
    state.message_hits.clear();
    state.transcript_viewport = Some(inner);
    if inner.width >= 40 {
        inner.x += 1;
        inner.width -= 2;
    }
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let width = usize::from(inner.width);
    let mut lines = VecDeque::new();
    let mut line_count = 0;
    let mut headers = Vec::new();
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
        let role_label = match message.role {
            Role::User => "You",
            Role::Assistant => "Saya",
            Role::Tool => "Tool",
            Role::Thinking => "Thinking",
            Role::System => "System",
        };
        let can_fold = foldable(message);
        let expanded = state.expanded_messages.contains(&message.id);
        let summary = if message.role == Role::Tool {
            summarize(&message.text)
        } else {
            None
        };
        let mut heading = Line::from(vec![
            Span::styled(
                if can_fold {
                    if expanded { "▼ " } else { "▶ " }
                } else {
                    ""
                },
                Style::default().fg(MUTED),
            ),
            Span::styled(
                role_label,
                role_style(message.role).add_modifier(Modifier::BOLD),
            ),
        ]);
        if message.role == Role::Thinking {
            let status = if message.streaming {
                "running"
            } else if message.label == "unavailable" {
                "unavailable"
            } else {
                "done"
            };
            heading.spans.push(Span::styled(
                format!(" · {status}"),
                Style::default().fg(if message.streaming { BLUE } else { MUTED }),
            ));
            if message.truncated {
                heading
                    .spans
                    .push(Span::styled(" · truncated", Style::default().fg(AMBER)));
            }
        } else if let Some(summary) = &summary {
            let label = safe_line(&message.label);
            let (name, source) = label.split_once(" · ").unwrap_or((&label, ""));
            if !name.is_empty() {
                heading.spans.push(Span::styled(
                    format!(" · {name}"),
                    Style::default()
                        .fg(STRONG_FOREGROUND)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            if message.truncated || summary.truncated {
                heading
                    .spans
                    .push(Span::styled(" · truncated", Style::default().fg(AMBER)));
            }
            heading.spans.push(Span::styled(
                if summary.ok { " · ✓ " } else { " · ✕ " },
                Style::default().fg(if summary.ok { GREEN } else { RED }),
            ));
            heading.spans.push(Span::styled(
                summary.text.clone(),
                Style::default().fg(if summary.ok { FOREGROUND } else { RED }),
            ));
            if !source.is_empty() {
                heading.spans.push(Span::styled(
                    format!(" · {source}"),
                    Style::default().fg(MUTED),
                ));
            }
        } else {
            if message.streaming {
                heading
                    .spans
                    .push(Span::styled(" · streaming", Style::default().fg(MUTED)));
            }
            if message.truncated {
                heading
                    .spans
                    .push(Span::styled(" · truncated", Style::default().fg(AMBER)));
            }
            if !message.label.is_empty() && message.label != role_label {
                heading.spans.push(Span::styled(
                    format!(" · {}", safe_line(&message.label)),
                    Style::default().fg(MUTED),
                ));
            }
        }
        if can_fold {
            let mut remaining = width;
            for span in &mut heading.spans {
                let text = crop_display(&span.content, 0, remaining);
                remaining = remaining.saturating_sub(text.width());
                span.content = text.into();
            }
        }
        if message.role == Role::Tool {
            heading = heading.style(Style::default().bg(CODE_BACKGROUND));
        }
        let header_start = line_count;
        append_wrapped_line(
            heading,
            width,
            &mut lines,
            &mut line_count,
            MAX_RENDER_LINES,
        );
        if can_fold {
            headers.push((message.id, header_start..line_count));
        }
        if can_fold && !expanded {
            append_wrapped("", width, Style::default(), &mut lines, &mut line_count);
            continue;
        }
        let text = safe_text(&message.text);
        if message.role == Role::Assistant {
            for line in markdown_lines(&text) {
                append_wrapped_line(line, width, &mut lines, &mut line_count, MAX_RENDER_LINES);
            }
        } else {
            let style = match message.role {
                Role::Tool => Style::default().fg(CODE_FOREGROUND).bg(CODE_BACKGROUND),
                Role::Thinking => Style::default().fg(MUTED).bg(CODE_BACKGROUND),
                Role::System => Style::default().fg(MUTED),
                _ => Style::default().fg(FOREGROUND),
            };
            append_wrapped(&text, width, style, &mut lines, &mut line_count);
        }
        if message.truncated {
            append_wrapped(
                "[display truncated]",
                width,
                Style::default().fg(AMBER),
                &mut lines,
                &mut line_count,
            );
        }
        append_wrapped("", width, Style::default(), &mut lines, &mut line_count);
    }
    let retained_start = line_count.saturating_sub(lines.len());
    let omitted = usize::from(line_count > MAX_RENDER_LINES);
    if omitted > 0 {
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
    if let Some((id, row)) = state.fold_anchor.take()
        && let Some((_, range)) = headers.iter().find(|(header_id, _)| *header_id == id)
    {
        let header_row = range.start.saturating_sub(retained_start) + omitted;
        let offset = usize::from(row.saturating_sub(inner.y)).min(usize::from(inner.height - 1));
        let start = header_row.saturating_sub(offset);
        state.scroll = lines
            .len()
            .saturating_sub(start.saturating_add(usize::from(inner.height)));
    }
    state.scroll = state
        .scroll
        .min(lines.len().saturating_sub(usize::from(inner.height)));
    let end = lines.len().saturating_sub(state.scroll);
    let start = end.saturating_sub(usize::from(inner.height));
    for (id, range) in headers {
        if range.end <= retained_start {
            continue;
        }
        let header_start = range.start.saturating_sub(retained_start) + omitted;
        let header_end = range.end.saturating_sub(retained_start) + omitted;
        let visible_start = header_start.max(start);
        let visible_end = header_end.min(end);
        if visible_start < visible_end {
            state.message_hits.push(MessageHit {
                area: Rect::new(
                    inner.x,
                    inner.y + (visible_start - start) as u16,
                    inner.width,
                    (visible_end - visible_start) as u16,
                ),
                id,
            });
        }
    }
    for (row, line) in lines.into_iter().skip(start).take(end - start).enumerate() {
        frame.render_widget(
            line,
            Rect::new(inner.x, inner.y + row as u16, inner.width, 1),
        );
    }
}

fn draw_details(
    frame: &mut Frame<'_>,
    details: &Details,
    scroll: &mut usize,
    page_rows: &mut usize,
    area: Rect,
) {
    let title = safe_line(&details.title[..prefix_boundary(&details.title, 256)]);
    let title_style = Style::default()
        .fg(
            if title.contains("Approval") || title.contains("Question") {
                AMBER
            } else {
                FOREGROUND
            },
        )
        .add_modifier(Modifier::BOLD);
    let mut block = Block::default().border_style(Style::default().fg(BORDER));
    if area.height > 1 {
        block = block
            .borders(Borders::TOP)
            .title(Line::styled(format!(" {title} "), title_style));
    }
    let inner = block.inner(area);
    *page_rows = usize::from(inner.height).clamp(1, 10);
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
                    AMBER
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
    if area.height > 1
        && (truncated || cached_truncated)
        && !title.starts_with(TRUNCATED_DETAIL_PREFIX)
    {
        block = block.title(Line::styled(
            format!(" {TRUNCATED_DETAIL_PREFIX}{title} "),
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ));
    }
    if truncated {
        if lines.len() == MAX_RENDER_LINES {
            lines.pop_back();
        }
        lines.push_back(Line::styled(
            "[details display truncated]",
            Style::default().fg(AMBER),
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
    let accent = mode_color(&state.mode);
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(accent).bg(accent))
        .style(Style::default().fg(FOREGROUND).bg(INPUT_BACKGROUND));
    let mut inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height > 0 {
        inner.x += 1;
        inner.width = inner.width.saturating_sub(1);
    }
    if area.height > 2 && inner.width > 0 {
        frame.render_widget(
            Line::styled(
                "─".repeat(usize::from(inner.width)),
                Style::default().fg(INPUT_BORDER).bg(INPUT_BACKGROUND),
            ),
            Rect::new(inner.x, area.bottom() - 1, inner.width, 1),
        );
    }
    if inner.height > 1 {
        let label = if state.editor.truncated {
            "Message · paste truncated at 64 KiB"
        } else {
            "Message"
        };
        let hint = if state.editor.truncated || area.height <= 2 {
            Line::default()
        } else {
            editor_hint(inner.width)
        };
        draw_split_line(
            frame,
            Line::styled(
                label,
                Style::default()
                    .fg(if state.editor.truncated {
                        AMBER
                    } else {
                        STRONG_FOREGROUND
                    })
                    .add_modifier(Modifier::BOLD),
            ),
            hint,
            Rect::new(inner.x, inner.y, inner.width, 1),
            label.width(),
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
    if area.height > 2
        && state.editor.text().is_empty()
        && !state.editor.truncated
        && !state.completion.visible()
        && !matches!(
            state.status.as_str(),
            "Awaiting input" | "NeedsInput" | "Awaiting approval" | "NeedsConfirmation"
        )
    {
        frame.render_widget(
            Line::styled(
                crop_display("Describe a task…", 0, usize::from(inner.width)),
                Style::default().fg(MUTED).add_modifier(Modifier::ITALIC),
            ),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
    }
    if state.command_palette.is_none() {
        frame.set_cursor_position((
            inner.x + (cursor_column - first_column) as u16,
            inner.y + (cursor_row - first_row) as u16,
        ));
    }
}

fn editor_hint(width: u16) -> Line<'static> {
    if width < 38 {
        return Line::default();
    }
    let key = Style::default()
        .fg(STRONG_FOREGROUND)
        .bg(CODE_BACKGROUND)
        .add_modifier(Modifier::BOLD);
    let action = Style::default().fg(MUTED);
    let mut spans = vec![Span::styled("Enter", key), Span::styled(" send · ", action)];
    if width >= 64 {
        spans.push(Span::styled("Alt+Enter", key));
        spans.push(Span::styled(" newline · ", action));
    }
    spans.push(Span::styled("Ctrl+O", key));
    spans.push(Span::styled(" fold", action));
    Line::from(spans)
}

fn draw_command_palette(frame: &mut Frame<'_>, state: &mut State) {
    let Some(selected) = state.command_palette else {
        state.palette_first = 0;
        return;
    };
    let area = frame.area();
    let width = area.width.saturating_sub(2).min(88);
    let height = area.height.min(14);
    if width < 4 || height < 4 {
        return;
    }
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    let compact = width < 38;
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().fg(FOREGROUND).bg(SIDEBAR_BACKGROUND))
        .title(Line::styled(
            " Commands ",
            Style::default()
                .fg(STRONG_FOREGROUND)
                .add_modifier(Modifier::BOLD),
        ));
    if compact {
        block = block.title_bottom(Line::styled(" Esc close ", Style::default().fg(MUTED)));
    } else {
        block = block.title(
            Line::styled(
                format!(" {}/{} ", selected + 1, COMMANDS.len()),
                Style::default().fg(MUTED),
            )
            .right_aligned(),
        );
    }
    let inner = block.inner(popup);
    let rows = usize::from(inner.height.saturating_sub(1)).min(COMMANDS.len());
    state.palette_first = state.palette_first.min(COMMANDS.len().saturating_sub(rows));
    if selected < state.palette_first {
        state.palette_first = selected;
    } else if selected >= state.palette_first + rows {
        state.palette_first = selected + 1 - rows;
    }
    let first = state.palette_first;
    let text_width = usize::from(inner.width.saturating_sub(2));
    let command_width = COMMANDS
        .iter()
        .map(|command| command.text.width())
        .max()
        .unwrap_or(0)
        .min(text_width);
    if height == area.height && area.width <= 22 {
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default().style(Style::default().fg(FOREGROUND).bg(BACKGROUND)),
            area,
        );
    }
    frame.render_widget(Clear, popup);
    frame.render_widget(block, popup);
    for (row, command) in COMMANDS.iter().skip(first).take(rows).enumerate() {
        let index = first + row;
        let active = index == selected;
        let style = if active {
            Style::default()
                .fg(BACKGROUND)
                .bg(BLUE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(FOREGROUND).bg(SIDEBAR_BACKGROUND)
        };
        let text = crop_display(command.text, 0, command_width);
        let mut line = Line::from(vec![
            Span::raw(if active { "› " } else { "  " }),
            Span::raw(text.clone()),
        ])
        .style(style);
        if text_width >= command_width + 5 {
            line.spans.push(Span::raw(
                " ".repeat(command_width.saturating_sub(text.width()) + 2),
            ));
            line.spans.push(Span::styled(
                crop_display(command.description, 0, text_width - command_width - 2),
                Style::default().fg(if active { BACKGROUND } else { MUTED }),
            ));
        }
        let area = Rect::new(inner.x, inner.y + row as u16, inner.width, 1);
        frame.render_widget(line, area);
        state.palette_hits.push((area, index));
    }
    frame.render_widget(
        Line::styled(
            if compact {
                "↑↓ · Enter"
            } else {
                "↑↓ select · Enter run · Esc close"
            },
            Style::default().fg(MUTED),
        ),
        Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
    );
}

fn draw_completion(frame: &mut Frame<'_>, state: &mut State, area: Rect) {
    if state.command_palette.is_some()
        || !state.completion.visible()
        || area.height < 3
        || area.width < 4
    {
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
    state.completion_viewport = Some(popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Command suggestions ")
        .title_bottom(" ↑↓ Tab/→ Esc ")
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(SIDEBAR_BACKGROUND));
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
                    .bg(BLUE)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(FOREGROUND).bg(SIDEBAR_BACKGROUND)
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
    for line in text.split('\n') {
        append_wrapped_line(
            Line::styled(line.to_owned(), style),
            width,
            lines,
            count,
            MAX_RENDER_LINES,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        commands::Suggestion,
        theme::{HEADING, INLINE_CODE},
    };
    use ratatui::{
        Terminal,
        backend::{Backend, TestBackend},
        style::Color,
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
        let collapsed = render(&mut state, 80, 24);
        assert!(collapsed.contains("truncated"));
        assert!(!collapsed.contains("display truncated"));
        assert!(state.toggle_latest());
        state.scroll_to_bottom();
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
        assert_eq!((cursor.x, cursor.y), (7, 20));
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
                .any(|cell| { cell.symbol() == "›" && cell.bg == BLUE })
        );
        let image = render(&mut state, 120, 24);
        assert!(image.contains("Command suggestions"));
        assert!(image.contains("Tab/→ Esc"));
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
        assert_eq!(terminal.backend().buffer()[(7, 20)].symbol(), " ");
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
        assert_eq!(row, 8);
        assert_eq!(buffer[(3, row)].bg, BLUE);
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
            .draw(|frame| draw_editor(frame, &state, frame.area()))
            .unwrap();
        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        assert_eq!((cursor.x, cursor.y), (5, 1));
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(2, 1)].symbol(), " ");
        assert_eq!(buffer[(3, 1)].symbol(), "界");
        assert_eq!(buffer[(5, 1)].symbol(), "a");
        assert_eq!(buffer[(5, 1)].fg, MUTED);
        assert_eq!(state.editor.text(), "/界界界");
        state.editor.move_left();
        terminal
            .draw(|frame| draw_editor(frame, &state, frame.area()))
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
    fn session_layout_keeps_status_input_and_footer_in_their_observable_regions() {
        let mut state = State {
            workspace: "/tmp/project".into(),
            model: "configured-model".into(),
            context_window_tokens: Some(128_000),
            context_input_tokens: Some(64_000),
            ..State::default()
        };
        state.editor.set_text("hello");
        for (width, height, status_row, title_row, input_row, main_width) in [
            (80, 24, 18, 19, 20, 80),
            (60, 18, 12, 13, 14, 60),
            (20, 6, 2, 3, 4, 20),
            (110, 32, 26, 27, 28, 110),
            (120, 40, 34, 35, 36, 90),
            (160, 45, 39, 40, 41, 120),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let cursor = terminal.backend_mut().get_cursor_position().unwrap();
            assert_eq!((cursor.x, cursor.y), (8, input_row));
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(0, 0)].bg, BACKGROUND);
            assert!(row_text(buffer, 0).contains("Saya Harness"));
            assert!(row_text(buffer, 0).contains("Ctrl+P"));
            assert!(row_text(buffer, 1).contains("/tmp/project"));
            assert!(row_text(buffer, status_row).contains("✓ Ready"));
            let status = (0..main_width)
                .map(|column| buffer[(column, status_row)].symbol())
                .collect::<String>();
            assert!(status.ends_with("auto│"));
            assert_eq!(buffer[(3, title_row)].symbol(), "M");
            assert_eq!(buffer[(1, title_row)].symbol(), "│");
            assert_eq!(buffer[(1, title_row)].fg, BLUE);
            assert_eq!(buffer[(1, title_row)].bg, BLUE);
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
    fn resizing_across_former_breakpoints_keeps_one_reference_layout() {
        let mut state = State {
            usage: "wide-usage-marker".into(),
            ..State::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(99, 23)).unwrap();
        for (width, height) in [
            (99, 23),
            (100, 23),
            (100, 24),
            (99, 24),
            (110, 32),
            (120, 40),
            (160, 45),
            (100, 24),
            (99, 23),
        ] {
            terminal.backend_mut().resize(width, height);
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let buffer = terminal.backend().buffer();
            let image = buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(row_text(buffer, 0).contains("Saya Harness"));
            assert!(row_text(buffer, 0).contains(env!("CARGO_PKG_VERSION")));
            assert!(row_text(buffer, 1).starts_with("┌ • . "));
            assert!(row_text(buffer, height - 6).contains("✓ Ready"));
            assert_eq!(buffer[(3, height - 5)].symbol(), "M");
            assert_eq!(buffer[(1, height - 5)].fg, BLUE);
            assert_eq!(buffer[(3, height - 4)].bg, INPUT_BACKGROUND);
            assert!(row_text(buffer, height - 1).starts_with("└WORK"));
            assert!(row_text(buffer, height - 1).contains("input"));
            assert!(!image.contains("SAYA"));
            assert!(!image.contains("wide-usage-marker"));
            assert!(!image.contains("Conversation"));
            assert!(row_text(buffer, height - 5).contains("Enter send"));
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
    fn multiline_editor_scrolls_within_its_gray_input_and_keeps_safety_markers() {
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
        assert!(row_text(buffer, 10).contains("Awaiting approval"));
        assert!(row_text(buffer, 10).ends_with("Manual Approval│"));
        assert_eq!(buffer[(3, 11)].symbol(), "M");
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
        for (width, height) in [(80, 24), (60, 18), (110, 32), (120, 40)] {
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
                    .any(|cell| cell.symbol() == "m" && cell.fg == AMBER)
            );
        }
        state.thinking_effort = None;
        assert!(!render(&mut state, 110, 32).contains(" max"));
    }

    #[test]
    fn wide_details_and_completion_share_the_session_body_without_covering_the_editor() {
        let mut state = State::default();
        state.push_message(Role::User, "You", "conversation marker");
        state.set_details(Details {
            title: "Approval required".into(),
            lines: vec!["operation marker".into()],
        });
        state.editor.set_text("/per");
        state.completion.update("/per", 4, true);
        for (width, main_width) in [(99, 99), (100, 100), (110, 110), (120, 90), (160, 120)] {
            let mut terminal = Terminal::new(TestBackend::new(width, 32)).unwrap();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let cursor = terminal.backend_mut().get_cursor_position().unwrap();
            assert_eq!((cursor.x, cursor.y), (7, 28));
            let buffer = terminal.backend().buffer();
            let right_width = ((main_width - 2) / 3).clamp(32, 48);
            let details_x = if main_width >= 100 {
                main_width - 1 - right_width
            } else {
                1
            };
            let heading = (details_x..main_width - 1)
                .map(|column| buffer[(column, 2)].symbol())
                .collect::<String>();
            assert!(heading.contains("Approval required"));
            assert!(row_text(buffer, 3).contains("operation marker"));
            if main_width >= 100 {
                assert!(row_text(buffer, 2).contains("You"));
                assert!(row_text(buffer, 3).contains("conversation marker"));
            }
            assert!(
                buffer
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .contains("Command suggestions")
            );
            assert_eq!(buffer[(cursor.x, cursor.y)].symbol(), "m");
            assert_eq!(buffer[(cursor.x, cursor.y)].bg, INPUT_BACKGROUND);
            assert_eq!(state.editor.text(), "/per");
            assert_eq!(state.details.as_ref().unwrap().title, "Approval required");
        }
    }

    #[test]
    fn sidebar_is_responsive_and_labels_unknown_extensions_without_invented_state() {
        let mut state = State {
            session_title: "Sidebar session".into(),
            workspace: "/tmp/workspace".into(),
            context_window_tokens: Some(128_000),
            ..State::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(119, 40)).unwrap();
        for (width, height, main_width) in [
            (119, 40, 119),
            (120, 40, 90),
            (120, 23, 120),
            (120, 24, 90),
            (160, 45, 120),
            (110, 32, 110),
            (119, 24, 119),
        ] {
            terminal.backend_mut().resize(width, height);
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(main_width - 1, height - 1)].symbol(), "┘");
            assert!(row_text(buffer, 0).starts_with("Saya Harness"));
            if main_width < width {
                assert_eq!(buffer[(main_width, 0)].bg, SIDEBAR_BACKGROUND);
                for (row, text) in [
                    (0, "Sidebar session"),
                    (2, "Context · Unknown"),
                    (3, "Window 128k"),
                    (4, "Unknown before request"),
                    (15, "Extensions"),
                    (16, "MCP  Not implemented"),
                    (17, "Plugins  Not implemented"),
                    (18, "Skills —"),
                    (height - 1, "/tmp/workspace"),
                ] {
                    let actual = (main_width..width)
                        .map(|column| buffer[(column, row)].symbol())
                        .collect::<String>();
                    assert!(
                        actual.contains(text),
                        "{width}x{height} row {row}: {actual}"
                    );
                }
            } else {
                assert_eq!(buffer[(width - 1, 0)].bg, BACKGROUND);
                assert!(!row_text(buffer, 2).contains("Context"));
            }
        }
    }

    #[test]
    fn sidebar_context_rows_are_bounded_safe_and_keep_small_proportions_visible() {
        let mut state = State {
            context_window_tokens: Some(128_000),
            context_breakdown: vec![
                ContextEntry {
                    label: "Skills".into(),
                    tokens: 128,
                },
                ContextEntry {
                    label: "Conversation".into(),
                    tokens: 64_000,
                },
            ],
            skills_count: Some(3),
            ..State::default()
        };
        let lines = context_rows(&state, 26);
        assert!(lines[0].to_string().contains("0.1%"));
        assert!(lines[0].to_string().contains('▏'));
        assert!(lines[1].to_string().contains("50%"));
        assert!(lines.iter().all(|line| line.width() <= 26));
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(row_text(buffer, 2).contains("Context · Estimated"));
        assert!(row_text(buffer, 13).contains("Total ~64.1k 50.1%"));
        assert!(row_text(buffer, 18).contains("Skills 3 discovered"));
        state.context_breakdown = (0..MAX_CONTEXT_ENTRIES + 10)
            .map(|_| ContextEntry {
                label: "unsafe\x1b[2J\u{202e}".into(),
                tokens: u64::MAX,
            })
            .collect();
        assert_eq!(context_rows(&state, 26).len(), MAX_CONTEXT_ENTRIES);
        assert!(
            context_rows(&state, 26)
                .iter()
                .all(|line| line.width() <= 26)
        );
        let image = render(&mut state, 120, 40);
        assert!(image.contains("Additional entries omitted"));
        assert!(!image.contains('\x1b'));
        assert!(!image.contains('\u{202e}'));
    }

    #[test]
    fn context_bars_and_percentages_handle_fractional_zero_unknown_and_overflow_inputs() {
        for (tokens, expected) in [
            (0, "0%"),
            (1, "<0.1%"),
            (128, "0.1%"),
            (64_000, "50%"),
            (128_000, "100%"),
        ] {
            assert_eq!(sidebar_percentage(tokens, Some(128_000)), expected);
        }
        assert_eq!(sidebar_percentage(u64::MAX, Some(0)), "—");
        assert_eq!(sidebar_percentage(u64::MAX, Some(1)), ">100%");
        assert_eq!(
            context_bar(128, Some(128_000), 5),
            ("▏".into(), "░░░░".into())
        );
        assert_eq!(
            context_bar(64_000, Some(128_000), 5),
            ("██▌".into(), "░░".into())
        );
        assert_eq!(
            context_bar(0, Some(128_000), 5),
            (String::new(), "░░░░░".into())
        );
        assert_eq!(
            context_bar(u64::MAX, Some(0), 5),
            (String::new(), "─────".into())
        );
        assert_eq!(
            context_bar(u64::MAX, Some(1), 5),
            ("█████".into(), String::new())
        );
        assert_eq!(context_bar(1, Some(1), 0), (String::new(), String::new()));
    }

    #[test]
    fn tiny_details_show_scrollable_content_while_retaining_an_editable_draft() {
        let mut state = State {
            status: "Awaiting approval".into(),
            permissions: "Manual Approval".into(),
            ..State::default()
        };
        state.editor.set_text("yes");
        state.set_details(Details {
            title: "Approval required".into(),
            lines: vec![
                "Tool read_file".into(),
                "Call c1".into(),
                "{\"path\":\"ok\"}".into(),
            ],
        });
        for height in [6, 7] {
            state.details_scroll = 0;
            let mut terminal = Terminal::new(TestBackend::new(20, height)).unwrap();
            for expected in ["Tool read_file", "Call c1", "{\"path\":\"ok\"}"] {
                terminal.draw(|frame| draw(frame, &mut state)).unwrap();
                let cursor = terminal.backend_mut().get_cursor_position().unwrap();
                assert_eq!((cursor.x, cursor.y), (6, height - 2));
                let buffer = terminal.backend().buffer();
                assert!(row_text(buffer, 2).contains(expected));
                assert!(row_text(buffer, 3).contains("Approval"));
                assert_eq!(buffer[(3, height - 2)].symbol(), "y");
                assert_eq!(buffer[(3, height - 2)].bg, INPUT_BACKGROUND);
                assert_eq!(state.details_page_rows, 1);
                state.details_scroll += state.details_page_rows;
            }
        }
    }

    #[test]
    fn message_headers_separate_role_metadata_and_literal_tool_cards() {
        let mut state = State::default();
        state.push_message(Role::User, "You", "**plain user text**");
        state.push_message(Role::Tool, "read_file", "`literal`\n# raw heading");
        state.push_message(Role::Assistant, "example-model", "answer");
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal
            .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert!(row_text(buffer, 0).starts_with(" You"));
        assert_eq!(buffer[(1, 0)].fg, BLUE);
        assert!(buffer[(1, 0)].modifier.contains(Modifier::BOLD));
        assert!(row_text(buffer, 1).contains("**plain user text**"));
        assert!(row_text(buffer, 3).contains("Tool · read_file"));
        assert_eq!(buffer[(1, 3)].fg, AMBER);
        assert_eq!(buffer[(8, 3)].fg, MUTED);
        for row in 3..=5 {
            assert_eq!(buffer[(1, row)].bg, CODE_BACKGROUND);
            assert_eq!(buffer[(78, row)].bg, CODE_BACKGROUND);
        }
        assert!(row_text(buffer, 4).contains("`literal`"));
        assert!(row_text(buffer, 5).contains("# raw heading"));
        assert!(row_text(buffer, 7).contains("Saya · example-model"));
        assert_eq!(buffer[(1, 7)].fg, FOREGROUND);
        assert_eq!(buffer[(8, 7)].fg, MUTED);
        assert_eq!(state.messages[0].text, "**plain user text**");
        assert_eq!(state.messages[1].text, "`literal`\n# raw heading");
    }

    #[test]
    fn assistant_markdown_uses_theme_and_preserves_source_and_code_card_width() {
        let source = "## Heading\nUse `input` and **care**.\n> quoted\n- item\n```rust\nlet x = \"**literal**\";\n```\nunsafe\x1b[2J";
        let mut state = State::default();
        state.push_message(Role::Assistant, "Saya", source);
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal
            .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert!(row_text(buffer, 1).starts_with(" Heading"));
        assert_eq!(buffer[(1, 1)].fg, HEADING);
        assert!(buffer[(1, 1)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(5, 2)].fg, INLINE_CODE);
        assert_eq!(buffer[(15, 2)].fg, AMBER);
        assert_eq!(buffer[(1, 3)].symbol(), "│");
        assert_eq!(buffer[(1, 3)].fg, MUTED);
        assert_eq!(buffer[(1, 4)].fg, AMBER);
        for row in 5..=7 {
            assert_eq!(buffer[(78, row)].bg, CODE_BACKGROUND);
        }
        assert!(row_text(buffer, 6).contains("**literal**"));
        assert_eq!(buffer[(1, 6)].fg, CODE_FOREGROUND);
        assert!(row_text(buffer, 8).contains("\\u{1b}[2J"));
        assert_eq!(state.messages[0].text, source);
        assert!(state.last_line_count < MAX_RENDER_LINES);
    }

    #[test]
    fn status_icons_distinguish_attention_failure_and_progress_without_relayout() {
        let mut state = State::default();
        for (status, expected) in [
            ("Ready", GREEN),
            ("Completed", GREEN),
            ("Running", BLUE),
            ("Reviewing", BLUE),
            ("Awaiting approval", AMBER),
            ("Awaiting input", AMBER),
            ("NeedsConfirmation", AMBER),
            ("Failed", RED),
            ("Cancelled", MUTED),
        ] {
            state.status = status.into();
            let mut terminal = Terminal::new(TestBackend::new(60, 1)).unwrap();
            terminal
                .draw(|frame| draw_status(frame, &state, frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(0, 0)].fg, expected, "{status}");
            assert_eq!(buffer[(2, 0)].fg, MUTED, "{status}");
            assert!(row_text(buffer, 0).ends_with("auto"));
        }
    }

    #[test]
    fn empty_draft_mode_hint_yields_to_palette_and_completion() {
        let mut state = State::default();
        for (width, hint) in [
            (80, "Tab mode · Ctrl+P commands"),
            (60, "Tab mode · Ctrl+P"),
            (30, "Ctrl+P"),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(width, 18)).unwrap();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            assert!(row_text(terminal.backend().buffer(), 0).contains(hint));
        }
        state.set_details(Details {
            title: "Commands".into(),
            lines: vec!["palette".into()],
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert!(row_text(terminal.backend().buffer(), 0).contains("Tab mode"));
        state.command_palette = Some(0);
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert!(!row_text(terminal.backend().buffer(), 0).contains("Tab mode"));
        state.command_palette = None;
        state.details = None;
        state.editor.set_text("/per");
        state.completion.update("/per", 4, true);
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let top = row_text(terminal.backend().buffer(), 0);
        assert!(top.contains("↑↓ Tab/→ Esc"));
        assert!(!top.contains("Tab mode"));
    }

    #[test]
    fn thinking_and_long_tools_fold_by_stable_id_and_keep_stream_updates() {
        let mut state = State::default();
        let id = state.push_message(
            Role::Thinking,
            "provider thought text",
            "private thought marker",
        );
        state.append_message(id, " · more");
        let image = render(&mut state, 80, 24);
        assert!(image.contains("▶ Thinking"));
        assert!(image.contains("▶ Thinking · running"));
        assert!(!image.contains("private thought marker"));
        let header = state.message_hits[0].area;
        assert!(state.toggle_at(header.x, header.y));
        assert!(render(&mut state, 80, 24).contains("private thought marker"));
        state.append_message(id, " · streamed addition");
        assert!(render(&mut state, 80, 24).contains("streamed addition"));
        assert!(state.expanded_messages.contains(&id));
        state.finish_message(id);
        assert!(render(&mut state, 80, 24).contains("Thinking · done"));
        let body = state.message_hits[0].area.bottom();
        assert!(!state.toggle_at(header.x, body));
        assert!(state.toggle_latest());
        assert!(!render(&mut state, 80, 24).contains("private thought marker"));

        for (text, expected) in [
            ("x".repeat(512), false),
            ("x".repeat(513), true),
            ("a\nb\nc\nd\ne\nf".into(), false),
            ("a\nb\nc\nd\ne\nf\ng".into(), true),
        ] {
            let tool_id = state.push_message(Role::Tool, "exec · call-1", &text);
            assert_eq!(foldable(state.messages.back().unwrap()), expected);
            assert_eq!(state.messages.back().unwrap().text, text);
            if expected {
                assert!(state.toggle_latest());
                assert!(state.expanded_messages.contains(&tool_id));
            }
        }
    }

    #[test]
    fn header_hits_ignore_forged_body_popup_occlusion_and_stale_resizes() {
        let mut state = State::default();
        state.push_message(Role::User, "You", "▶ Thinking · forged\n▶ Tool · forged");
        let thought = state.push_message(Role::Thinking, "provider", "actual source");
        render(&mut state, 80, 24);
        let hit = state.message_hits[0].area;
        assert!(!state.toggle_at(hit.x, hit.y - 2));
        assert!(state.expanded_messages.is_empty());
        assert!(state.toggle_at(hit.x, hit.y));
        assert!(state.expanded_messages.contains(&thought));
        render(&mut state, 20, 6);
        assert!(state.message_hits.is_empty());
        assert!(!state.toggle_at(hit.x, hit.y));

        state.clear_messages();
        state.push_message(Role::Thinking, "provider", "new thought");
        state.editor.set_text("/per");
        state.completion.update("/per", 4, true);
        render(&mut state, 80, 12);
        let hit = state.message_hits[0].area;
        assert!(contains(state.completion_viewport, hit.x + 1, hit.y));
        assert!(!state.toggle_at(hit.x + 1, hit.y));
        assert!(!state.scroll_at(hit.x + 1, hit.y, true));
        assert!(state.expanded_messages.is_empty());
    }

    #[test]
    fn fold_anchor_keeps_opened_header_visible_and_eviction_releases_fold_state() {
        let mut state = State::default();
        let id = state.push_message(Role::Tool, "exec · call-long", "line\n".repeat(200));
        state.push_message(Role::Assistant, "Saya", "result");
        render(&mut state, 80, 24);
        let row = state.message_hits[0].area.y;
        assert!(state.toggle_at(3, row));
        render(&mut state, 80, 24);
        assert_eq!(state.message_hits[0].id, id);
        assert_eq!(state.message_hits[0].area.y, row);
        assert!(state.scroll > 0);
        state.scroll_to_bottom();
        render(&mut state, 80, 24);
        assert!(state.message_hits.is_empty());
        assert!(state.toggle_latest());
        render(&mut state, 80, 24);
        assert_eq!(state.message_hits[0].id, id);
        assert!(state.toggle_latest());
        assert!(state.expanded_messages.contains(&id));
        for _ in 0..MAX_MESSAGES {
            state.push_message(Role::User, "You", "next");
        }
        assert!(!state.expanded_messages.contains(&id));
        assert!(state.expanded_messages.len() <= MAX_MESSAGES);
        state.clear_messages();
        assert!(state.expanded_messages.is_empty());
        assert!(state.message_hits.is_empty());
        assert!(!state.toggle_latest());
    }

    #[test]
    fn mouse_wheel_targets_only_actual_conversation_and_details_viewports() {
        let mut state = State::default();
        state.push_message(Role::Assistant, "Saya", "line\n".repeat(200));
        state.set_details(Details {
            title: "Approval required".into(),
            lines: (0..100).map(|index| format!("line {index}")).collect(),
        });
        render(&mut state, 160, 45);
        let transcript = state.transcript_viewport.unwrap();
        let details = state.details_viewport.unwrap();
        assert!(state.scroll_at(transcript.x + 1, transcript.y + 1, true));
        assert_eq!(state.scroll, 3);
        assert_eq!(state.details_scroll, 0);
        assert!(state.scroll_at(transcript.x + 1, transcript.y + 1, false));
        assert_eq!(state.scroll, 0);
        assert!(state.scroll_at(details.x + 1, details.y + 1, false));
        assert_eq!(state.details_scroll, 3);
        assert!(state.scroll_at(details.x + 1, details.y + 1, true));
        assert_eq!(state.details_scroll, 0);
        for (x, y) in [(0, 0), (3, 39), (3, 41), (121, 4)] {
            assert!(!state.scroll_at(x, y, true));
        }
        render(&mut state, 20, 6);
        assert!(state.transcript_viewport.is_none());
        assert!(state.scroll_at(2, 2, false));
        assert_eq!(state.details_scroll, 3);
        assert!(!state.toggle_at(2, 2));
    }

    #[test]
    fn hit_mapping_tracks_only_headers_retained_inside_the_render_line_budget() {
        let mut state = State::default();
        let source = "thought body\n".repeat(MAX_RENDER_LINES + 32);
        let id = state.push_message(Role::Thinking, "provider", &source);
        assert!(state.toggle_latest());
        state.scroll_to_bottom();
        render(&mut state, 80, 24);
        assert!(state.message_hits.is_empty());
        state.scroll_up(usize::MAX);
        assert!(render(&mut state, 80, 24).contains("earlier display lines omitted"));
        assert!(state.message_hits.is_empty());
        assert!(state.toggle_latest());
        render(&mut state, 80, 24);
        assert_eq!(state.message_hits.len(), 1);
        assert_eq!(state.message_hits[0].id, id);
        assert_eq!(state.messages[0].text, source);

        state.clear_messages();
        state.push_message(
            Role::Assistant,
            "Saya",
            "▶ Thinking forged\n".repeat(MAX_RENDER_LINES + 32),
        );
        let actual = state.push_message(Role::Thinking, "provider", "hidden body");
        render(&mut state, 80, 24);
        let hit = state.message_hits[0].area;
        assert_eq!(state.message_hits.len(), 1);
        assert_eq!(state.message_hits[0].id, actual);
        assert!(!state.toggle_at(hit.x, hit.y - 2));
        assert!(state.toggle_at(hit.x, hit.y));
        assert!(state.expanded_messages.contains(&actual));
    }

    #[test]
    fn folded_headers_reserve_visible_state_before_clipping_long_untrusted_labels() {
        let mut state = State::default();
        state.push_message(
            Role::Tool,
            "long-label-".repeat(30),
            "x".repeat(MAX_MESSAGE_BYTES + 1),
        );
        let image = render(&mut state, 80, 24);
        assert!(image.contains("▶ Tool · truncated"));
        assert_eq!(state.message_hits[0].area.height, 1);
        state.clear_messages();
        let id = state.push_message(Role::Thinking, "long-label-".repeat(30), "hidden");
        state.append_message(id, "");
        assert!(render(&mut state, 30, 10).contains("▶ Thinking · running"));
        assert_eq!(state.message_hits[0].area.height, 1);
    }

    #[test]
    fn sidebar_tasks_keep_real_statuses_counts_and_bounded_omission_at_both_heights() {
        let mut state = State::default();
        assert!(render(&mut state, 120, 40).contains("No tasks"));
        state.todos = (0..64)
            .map(|index| TodoItem {
                id: format!("task-{index}"),
                content: if index == 1 {
                    "unsafe\x1b[2J\u{202e} 中文👩‍💻".into()
                } else {
                    format!("Task {index} {}", "content ".repeat(20))
                },
                status: match index {
                    0 => TodoStatus::InProgress,
                    1 => TodoStatus::Completed,
                    _ => TodoStatus::Pending,
                },
            })
            .collect();
        for (height, tasks_row, extensions_row, first_task_row, omitted_row, omitted_count) in
            [(24, 12, 19, 13, 17, 60), (40, 20, 15, 21, 29, 56)]
        {
            let mut terminal = Terminal::new(TestBackend::new(120, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let buffer = terminal.backend().buffer();
            assert!(row_text(buffer, tasks_row).contains("Tasks · 1/64"));
            assert!(row_text(buffer, extensions_row).contains("Extensions"));
            assert!(
                row_text(buffer, omitted_row).contains(&format!("+{omitted_count} more tasks"))
            );
            assert_eq!(buffer[(91, first_task_row)].symbol(), "◐");
            assert_eq!(buffer[(91, first_task_row)].fg, BLUE);
            assert_eq!(buffer[(93, first_task_row)].fg, STRONG_FOREGROUND);
            assert!(
                buffer[(93, first_task_row)]
                    .modifier
                    .contains(Modifier::BOLD)
            );
            assert_eq!(buffer[(91, first_task_row + 1)].symbol(), "✓");
            assert_eq!(buffer[(91, first_task_row + 1)].fg, GREEN);
            assert_eq!(buffer[(93, first_task_row + 1)].fg, MUTED);
            let image = buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(image.contains('…'));
            assert!(!image.contains('\x1b'));
            assert!(!image.contains('\u{202e}'));
        }
        state.clear_messages();
        assert!(state.todos.is_empty());
    }

    #[test]
    fn canonical_short_tools_fold_with_real_summary_and_preserve_the_original_payload() {
        let source = serde_json::json!({
            "ok": true,
            "data": {"todos": [{"id": "one", "content": "UNIQUE_TASK_PAYLOAD", "status": "pending"}]},
            "error": null,
            "truncated": false,
        }).to_string();
        assert!(source.len() < TOOL_COLLAPSE_BYTES);
        let mut state = State::default();
        let id = state.push_message(Role::Tool, "todo_write · call-summary · completed", &source);
        let mut terminal = Terminal::new(TestBackend::new(200, 8)).unwrap();
        terminal
            .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
            .unwrap();
        let header = row_text(terminal.backend().buffer(), 0);
        assert!(header.contains(
            "▶ Tool · todo_write · ✓ 1 task · 0 done/0 active · call-summary · completed"
        ));
        assert!(!render(&mut state, 110, 32).contains("UNIQUE_TASK_PAYLOAD"));
        assert!(state.toggle_latest());
        state.scroll_to_bottom();
        terminal
            .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
            .unwrap();
        assert!(row_text(terminal.backend().buffer(), 1).contains(&source));
        assert_eq!(state.messages[0].id, id);
        assert_eq!(
            state.messages[0].label,
            "todo_write · call-summary · completed"
        );
        assert_eq!(state.messages[0].text, source);
        assert!(state.toggle_at(3, 0));
        terminal
            .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
            .unwrap();
        assert!(row_text(terminal.backend().buffer(), 0).starts_with(" ▶ Tool"));
        assert!(!row_text(terminal.backend().buffer(), 1).contains("UNIQUE_TASK_PAYLOAD"));
    }

    #[test]
    fn tool_status_colors_use_canonical_result_and_keep_truncation_visible() {
        let mut state = State::default();
        state.push_message(
            Role::Tool,
            "read_file · call-error · completed",
            serde_json::json!({
                "ok": false, "data": null,
                "error": {"code": "io_error", "message": "missing file"}, "truncated": false,
            })
            .to_string(),
        );
        let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(
            row_text(buffer, 2)
                .contains("Tool · read_file · ✕ io_error · missing file · call-error")
        );
        let marker = buffer
            .content
            .iter()
            .find(|cell| cell.symbol() == "✕")
            .unwrap();
        assert_eq!(marker.fg, RED);
        state.clear_messages();
        state.push_message(Role::Tool, "read_file · call-cut · completed", serde_json::json!({
            "ok": true, "error": null, "truncated": true,
            "data": {"content": "first\n", "start_line": 1, "end_line": 1, "version": "v1", "truncated": true},
        }).to_string());
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(
            row_text(buffer, 2)
                .contains("Tool · read_file · truncated · ✓ lines 1–1 · 6 bytes · call-cut")
        );
        assert!(
            buffer
                .content
                .iter()
                .any(|cell| cell.symbol() == "✓" && cell.fg == GREEN)
        );
        assert!(
            buffer
                .content
                .iter()
                .any(|cell| cell.symbol() == "t" && cell.fg == AMBER)
        );
    }

    #[test]
    fn thinking_headers_are_compact_and_keep_availability_and_truncation_state() {
        let mut state = State::default();
        let id = state.push_message(
            Role::Thinking,
            "provider thought text",
            "retained thought body",
        );
        state.append_message(id, "");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(
            row_text(buffer, 2).trim_matches('│').trim(),
            "▶ Thinking · running"
        );
        assert_eq!(buffer[(2, 2)].fg, MUTED);
        assert_eq!(buffer[(2, 2)].bg, BACKGROUND);
        assert!(!row_text(buffer, 2).contains("provider"));
        assert!(!row_text(buffer, 2).contains("Ctrl+O"));
        state.finish_message(id);
        state.messages[0].truncated = true;
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert_eq!(
            row_text(terminal.backend().buffer(), 2)
                .trim_matches('│')
                .trim(),
            "▶ Thinking · done · truncated"
        );
        state.messages[0].label = "unavailable".into();
        state.messages[0].truncated = false;
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert_eq!(
            row_text(terminal.backend().buffer(), 2)
                .trim_matches('│')
                .trim(),
            "▶ Thinking · unavailable"
        );
        assert_eq!(state.messages[0].text, "retained thought body");
    }

    #[test]
    fn input_hints_brand_permissions_and_sidebar_hierarchy_fit_the_existing_regions() {
        let mut state = State {
            permissions: "Manual Approval".into(),
            ..State::default()
        };
        state.editor.set_text("draft");
        for (width, hint) in [
            (80, "Enter send · Alt+Enter newline · Ctrl+O fold"),
            (68, "Enter send · Alt+Enter newline · Ctrl+O fold"),
            (67, "Enter send · Ctrl+O fold"),
            (42, "Enter send · Ctrl+O fold"),
            (41, ""),
            (20, ""),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let buffer = terminal.backend().buffer();
            let title = row_text(buffer, 19);
            assert_eq!(buffer[(3, 19)].symbol(), "M");
            assert_eq!(buffer[(3, 20)].symbol(), "d");
            assert_eq!(buffer[(1, 19)].bg, BLUE);
            assert_eq!(buffer[(2, 19)].bg, INPUT_BACKGROUND);
            if hint.is_empty() {
                assert!(!title.contains("Enter send"));
            } else {
                assert!(title.contains(hint), "{width}: {title}");
            }
            assert_eq!(buffer[(0, 0)].fg, STRONG_FOREGROUND);
            assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
        }
        state.editor.truncated = true;
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(row_text(buffer, 35).contains("Message · paste truncated"));
        assert!(!row_text(buffer, 35).contains("Enter send"));
        for row in [2, 15, 20] {
            assert_eq!(buffer[(121, row)].fg, STRONG_FOREGROUND);
            assert!(buffer[(121, row)].modifier.contains(Modifier::BOLD));
        }
        for row in [1, 14, 19] {
            assert_eq!(buffer[(121, row)].symbol(), "─");
            assert_eq!(buffer[(121, row)].fg, BORDER);
        }
        let status = row_text(buffer, 34);
        let manual = status[..status.find("Manual Approval").unwrap()].width();
        assert_eq!(buffer[(manual as u16, 34)].fg, SOFT_AMBER);
        state.context_breakdown = vec![
            ContextEntry {
                label: "System".into(),
                tokens: 100,
            },
            ContextEntry {
                label: "Skills".into(),
                tokens: 0,
            },
        ];
        let rows = context_rows(&state, 38);
        assert_eq!(
            rows[0].spans.last().unwrap().style.fg,
            Some(STRONG_FOREGROUND)
        );
        assert_eq!(rows[1].spans.last().unwrap().style.fg, Some(MUTED));
    }

    #[test]
    fn composer_placeholder_is_visual_and_excludes_user_text_questions_and_approvals() {
        let mut state = State::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        assert_eq!((cursor.x, cursor.y), (3, 20));
        assert!(state.editor.text().is_empty());
        let buffer = terminal.backend().buffer();
        assert!(row_text(buffer, 20).contains("Describe a task…"));
        assert_eq!(buffer[(3, 20)].fg, MUTED);
        assert_eq!(buffer[(3, 20)].bg, INPUT_BACKGROUND);
        assert!(buffer[(3, 20)].modifier.contains(Modifier::ITALIC));
        assert_eq!(buffer[(3, 19)].fg, STRONG_FOREGROUND);
        assert!(buffer[(3, 19)].modifier.contains(Modifier::BOLD));

        state.editor.set_text("Describe a task…");
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(3, 20)].fg, FOREGROUND);
        assert!(!buffer[(3, 20)].modifier.contains(Modifier::ITALIC));
        assert_eq!(state.editor.text(), "Describe a task…");
        state.editor.clear();
        for status in [
            "Awaiting input",
            "NeedsInput",
            "Awaiting approval",
            "NeedsConfirmation",
        ] {
            state.status = status.into();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            assert!(
                !row_text(terminal.backend().buffer(), 20).contains("Describe a task…"),
                "{status}"
            );
            assert_eq!(terminal.backend().buffer()[(3, 20)].symbol(), " ");
        }
        state.status = "Ready".into();
        state.editor.truncated = true;
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(row_text(buffer, 19).contains("Message · paste truncated"));
        assert!(!row_text(buffer, 20).contains("Describe a task…"));
        assert_eq!(buffer[(3, 19)].fg, AMBER);
        assert!(buffer[(3, 19)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn composer_keycaps_and_padding_frame_multiline_unicode_and_completion_without_moving_cursor() {
        let mut state = State::default();
        state.editor.set_text("one\n界e\u{301}👩‍💻");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        assert_eq!((cursor.x, cursor.y), (8, 20));
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(3, 19)].symbol(), "o");
        assert_eq!(buffer[(3, 20)].symbol(), "界");
        assert_eq!(buffer[(5, 20)].symbol(), "e\u{301}");
        assert_eq!(buffer[(6, 20)].symbol(), "👩‍💻");
        assert_eq!(buffer[(3, 22)].symbol(), "─");
        assert_eq!(buffer[(3, 22)].fg, INPUT_BORDER);
        assert_eq!(buffer[(3, 22)].bg, INPUT_BACKGROUND);

        state.editor.set_text("/per");
        state.completion.update("/per", 4, true);
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        assert_eq!((cursor.x, cursor.y), (7, 20));
        assert_eq!(state.editor.text(), "/per");
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(7, 20)].symbol(), "m");
        assert_eq!(buffer[(7, 20)].fg, MUTED);
        assert!(!buffer[(7, 20)].modifier.contains(Modifier::ITALIC));
        assert!(!row_text(buffer, 20).contains("Describe a task…"));
        let title = row_text(buffer, 19);
        let enter = title[..title.find("Enter send").unwrap()].width() as u16;
        assert_eq!(buffer[(enter, 19)].fg, STRONG_FOREGROUND);
        assert_eq!(buffer[(enter, 19)].bg, CODE_BACKGROUND);
        assert!(buffer[(enter, 19)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(enter + 6, 19)].fg, MUTED);
        assert_eq!(buffer[(enter + 6, 19)].bg, INPUT_BACKGROUND);
        assert_eq!(buffer[(3, 22)].fg, INPUT_BORDER);
    }

    #[test]
    fn tiny_composers_and_long_pastes_keep_real_input_cells_and_warning_priority() {
        let mut state = State::default();
        let mut terminal = Terminal::new(TestBackend::new(20, 6)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        assert_eq!((cursor.x, cursor.y), (3, 4));
        assert_eq!(terminal.backend().buffer()[(3, 4)].symbol(), " ");
        assert!(!row_text(terminal.backend().buffer(), 3).contains("Enter"));
        assert!(!render(&mut state, 20, 6).contains("Describe a task…"));
        state.set_details(Details {
            title: "Approval required".into(),
            lines: vec!["Tool exec".into()],
        });
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        assert_eq!((cursor.x, cursor.y), (3, 4));
        assert_eq!(terminal.backend().buffer()[(3, 4)].symbol(), " ");
        assert!(!row_text(terminal.backend().buffer(), 4).contains("Message"));
        assert!(!row_text(terminal.backend().buffer(), 4).contains('─'));

        state.details = None;
        state.editor.insert(&"界".repeat(30_000));
        assert!(state.editor.truncated);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        assert!(cursor.x >= 3 && cursor.x < 79);
        assert_eq!(cursor.y, 20);
        let buffer = terminal.backend().buffer();
        assert!(row_text(buffer, 19).contains("Message · paste truncated at 64 KiB"));
        assert_eq!(buffer[(3, 19)].fg, AMBER);
        assert!(!row_text(buffer, 19).contains("Enter send"));
        assert!(!row_text(buffer, 20).contains("Describe a task…"));
        assert_eq!(buffer[(3, 22)].fg, INPUT_BORDER);
        assert!(
            state
                .editor
                .text()
                .is_char_boundary(state.editor.text().len())
        );
    }

    #[test]
    fn active_mode_colors_link_the_entire_input_stripe_and_footer_at_all_sizes() {
        for (mode, color) in [
            ("WORK", Color::Rgb(88, 166, 255)),
            ("CODE", Color::Rgb(125, 203, 155)),
            ("PLAN", Color::Rgb(187, 154, 247)),
            ("BUILD", Color::Rgb(246, 173, 85)),
            ("FUTURE", MUTED),
        ] {
            for (width, height, input_top, input_height) in [
                (80, 24, 19, 4),
                (60, 18, 13, 4),
                (20, 6, 3, 2),
                (120, 40, 35, 4),
                (160, 45, 40, 4),
            ] {
                let mut state = State {
                    mode: mode.into(),
                    model: "fixture-model".into(),
                    ..State::default()
                };
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| draw(frame, &mut state)).unwrap();
                let buffer = terminal.backend().buffer();
                for row in input_top..input_top + input_height {
                    assert_eq!(buffer[(1, row)].fg, color, "{mode}, {width}x{height}");
                    assert_eq!(buffer[(1, row)].bg, color, "{mode}, {width}x{height}");
                }
                for column in 1..=mode.len() as u16 {
                    assert_eq!(
                        buffer[(column, height - 1)].fg,
                        color,
                        "{mode}, {width}x{height}"
                    );
                }
                let model_column = mode.len() as u16 + 2;
                if buffer[(model_column, height - 1)].symbol() == "f" {
                    assert_eq!(buffer[(model_column, height - 1)].fg, FOREGROUND);
                }
                assert_eq!(state.mode, mode);
                assert!(state.editor.text().is_empty());
                if width == 20 {
                    state.set_details(Details {
                        title: "Approval required".into(),
                        lines: vec!["Review this operation".into()],
                    });
                    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
                    let buffer = terminal.backend().buffer();
                    assert_eq!(buffer[(1, 4)].fg, color);
                    assert_eq!(buffer[(1, 4)].bg, color);
                    assert_eq!(buffer[(1, 5)].fg, color);
                }
            }
        }
    }

    #[test]
    fn pending_mode_has_its_own_label_color_until_the_active_accent_changes_at_the_boundary() {
        for (current, next) in [
            ("WORK", "CODE"),
            ("CODE", "PLAN"),
            ("PLAN", "BUILD"),
            ("BUILD", "WORK"),
        ] {
            let mut state = State {
                mode: current.into(),
                pending_mode: Some(next.into()),
                ..State::default()
            };
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let buffer = terminal.backend().buffer();
            assert!(row_text(buffer, 23).contains(&format!("{current}→{next}")));
            assert_eq!(buffer[(1, 19)].bg, mode_color(current));
            assert_eq!(buffer[(1, 23)].fg, mode_color(current));
            let arrow = 1 + current.len() as u16;
            assert_eq!(buffer[(arrow, 23)].fg, MUTED);
            for column in arrow + 1..arrow + 1 + next.len() as u16 {
                assert_eq!(buffer[(column, 23)].fg, mode_color(next));
            }
            let mut tiny = Terminal::new(TestBackend::new(20, 6)).unwrap();
            tiny.draw(|frame| draw(frame, &mut state)).unwrap();
            assert_eq!(tiny.backend().buffer()[(1, 3)].bg, mode_color(current));
            assert_eq!(tiny.backend().buffer()[(1, 5)].fg, mode_color(current));

            state.mode = next.into();
            state.pending_mode = None;
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(1, 19)].bg, mode_color(next));
            assert_eq!(buffer[(1, 23)].fg, mode_color(next));
            assert!(!row_text(buffer, 23).contains('→'));
        }
    }

    #[test]
    fn command_modal_centers_catalog_rows_and_restores_the_real_editor_cursor() {
        let mut state = State::default();
        state.editor.set_text("draft界");
        state.set_details(Details {
            title: "Saved records".into(),
            lines: (0..80).map(|index| format!("record {index}")).collect(),
        });
        state.details_scroll = 3;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert!(terminal.backend().cursor_visible());
        assert_eq!(terminal.backend().cursor_position(), (10, 20).into());
        state.command_palette = Some(0);
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert!(!terminal.backend().cursor_visible());
        let buffer = terminal.backend().buffer();
        for (x, y, symbol) in [(1, 5, "╭"), (78, 5, "╮"), (1, 18, "╰"), (78, 18, "╯")] {
            assert_eq!(buffer[(x, y)].symbol(), symbol);
        }
        assert!(row_text(buffer, 5).contains("Commands"));
        assert!(row_text(buffer, 5).contains(&format!("1/{}", COMMANDS.len())));
        assert!(row_text(buffer, 6).contains(COMMANDS[0].text));
        assert!(row_text(buffer, 6).contains(COMMANDS[0].description));
        assert_eq!(buffer[(2, 6)].symbol(), "›");
        assert_eq!(buffer[(2, 6)].bg, BLUE);
        assert!(buffer[(2, 6)].modifier.contains(Modifier::BOLD));
        assert_eq!(state.palette_hits.len(), 11);
        assert_eq!(state.palette_index_at(2, 6), Some(0));
        for (x, y) in [(2, 5), (1, 6), (2, 17), (0, 0)] {
            assert_eq!(state.palette_index_at(x, y), None);
        }
        assert_eq!(state.details.as_ref().unwrap().title, "Saved records");
        assert_eq!(state.details_scroll, 3);
        state.command_palette = None;
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert!(terminal.backend().cursor_visible());
        assert_eq!(terminal.backend().cursor_position(), (10, 20).into());
        assert_eq!(state.editor.text(), "draft界");
        assert_eq!(state.details_scroll, 3);
        assert!(state.palette_hits.is_empty());
        assert!(row_text(terminal.backend().buffer(), 2).contains("Saved records"));
    }

    #[test]
    fn command_modal_clamps_selection_and_scrolls_through_the_shared_catalog() {
        let mut state = State {
            command_palette: Some(usize::MAX),
            ..State::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let last = COMMANDS.len() - 1;
        assert_eq!(state.command_palette, Some(last));
        assert_eq!(terminal.backend().buffer()[(36, 13)].symbol(), "╭");
        assert_eq!(terminal.backend().buffer()[(123, 26)].symbol(), "╯");
        assert_eq!(state.palette_hits.len(), 11);
        for (row, (area, index)) in state.palette_hits.iter().enumerate() {
            assert_eq!(*index, COMMANDS.len() - 11 + row);
            assert!(
                row_text(terminal.backend().buffer(), area.y)
                    .contains(COMMANDS[*index].text.trim_end())
            );
            assert_eq!(state.palette_index_at(area.x + 3, area.y), Some(*index));
        }
        assert_eq!(terminal.backend().buffer()[(37, 24)].symbol(), "›");
        assert!(state.palette_scroll(false));
        assert_eq!(state.command_palette, Some(last));
        assert!(state.palette_scroll(true));
        assert_eq!(state.command_palette, Some(last - 1));
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert_eq!(state.palette_hits.last().unwrap().1, last);
        assert_eq!(state.palette_first, COMMANDS.len() - 11);
        assert_eq!(terminal.backend().buffer()[(37, 23)].symbol(), "›");
        let middle = state.palette_hits[3];
        state.command_palette = state.palette_index_at(middle.0.x + 3, middle.0.y);
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert_eq!(state.palette_first, COMMANDS.len() - 11);
        assert_eq!(
            terminal.backend().buffer()[(middle.0.x, middle.0.y)].symbol(),
            "›"
        );
        assert_eq!(
            state.palette_index_at(middle.0.x, middle.0.y),
            Some(middle.1)
        );
        state.command_palette = Some(0);
        assert!(state.palette_scroll(true));
        assert_eq!(state.command_palette, Some(0));
        assert!(state.palette_scroll(false));
        assert_eq!(state.command_palette, Some(1));
        state.command_palette = None;
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert_eq!(state.palette_first, 0);
        assert!(!state.palette_scroll(false));
        assert_eq!(state.palette_index_at(37, 24), None);
    }

    #[test]
    fn command_modal_tiny_layout_keeps_selection_close_hint_and_fresh_resize_hits() {
        let mut state = State {
            command_palette: Some(COMMANDS.len() - 1),
            ..State::default()
        };
        state.editor.set_text("retained draft");
        let mut terminal = Terminal::new(TestBackend::new(20, 6)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(1, 0)].symbol(), "╭");
        assert_eq!(buffer[(18, 5)].symbol(), "╯");
        assert!(row_text(buffer, 0).contains("Commands"));
        assert!(row_text(buffer, 4).contains("↑↓ · Enter"));
        assert!(row_text(buffer, 5).contains("Esc close"));
        assert_eq!(buffer[(2, 3)].symbol(), "›");
        assert_eq!(state.palette_index_at(3, 3), Some(COMMANDS.len() - 1));
        assert_eq!(state.palette_hits.len(), 3);
        for row in 0..6 {
            assert_eq!(buffer[(0, row)].symbol(), " ");
            assert_eq!(buffer[(19, row)].symbol(), " ");
            assert_eq!(buffer[(0, row)].bg, BACKGROUND);
            assert_eq!(buffer[(19, row)].bg, BACKGROUND);
        }
        assert!(!terminal.backend().cursor_visible());
        terminal.backend_mut().resize(110, 32);
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert_eq!(state.palette_index_at(3, 3), None);
        assert_eq!(terminal.backend().buffer()[(11, 9)].symbol(), "╭");
        assert_eq!(state.palette_hits.len(), 11);
        assert_eq!(state.editor.text(), "retained draft");
        state.command_palette = Some(usize::MAX);
        render(&mut state, 1, 1);
        assert_eq!(state.command_palette, Some(COMMANDS.len() - 1));
        assert!(state.palette_hits.is_empty());
    }

    #[test]
    fn command_modal_blocks_backdrop_actions_and_suppresses_completion_overlay() {
        let mut state = State::default();
        let id = state.push_message(Role::Thinking, "provider", "thought\n".repeat(100));
        state.set_details(Details {
            title: "Approval required".into(),
            lines: (0..100).map(|index| format!("argument {index}")).collect(),
        });
        state.details_scroll = 4;
        state.editor.set_text("/per");
        state.completion.update("/per", 4, true);
        render(&mut state, 160, 45);
        state.command_palette = Some(0);
        let image = render(&mut state, 160, 45);
        let scroll = state.scroll;
        let details_scroll = state.details_scroll;
        assert!(!image.contains("Command suggestions"));
        assert!(state.completion.visible());
        assert!(state.completion_viewport.is_none());
        assert!(!state.toggle_latest());
        assert!(!state.toggle_at(2, 2));
        assert!(!state.scroll_at(3, 3, true));
        assert!(!state.scroll_at(82, 3, false));
        assert!(!state.expanded_messages.contains(&id));
        assert_eq!(state.scroll, scroll);
        assert_eq!(state.details_scroll, details_scroll);
        assert_eq!(state.editor.text(), "/per");
        assert_eq!(state.details.as_ref().unwrap().title, "Approval required");
        state.command_palette = None;
        assert!(render(&mut state, 160, 45).contains("Command suggestions"));
    }
}
