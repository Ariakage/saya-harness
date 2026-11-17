//! Pure, bounded TUI state and rendering. Runtime effects belong to the controller.
use crate::{
    attachments,
    commands::{COMMANDS, Completion},
    editor::{Editor, MAX_VISUAL_LINE_BYTES, display_grapheme, prefix_boundary, safe_text},
    i18n::{
        command, common, controller as controller_text, mode as mode_text,
        permissions as permission_text, status as status_text, tool as tool_text, view as ui_text,
    },
    rich_text::append_wrapped_line,
    theme::{
        AMBER, BACKGROUND, BLUE, BORDER, CODE_BACKGROUND, CODE_FOREGROUND, FOREGROUND, GREEN,
        HEADING, INPUT_BACKGROUND, INPUT_BORDER, INPUT_STRIPE, MUTED, RED, SIDEBAR_BACKGROUND,
        SOFT_AMBER, STRONG_FOREGROUND, mode_color,
    },
    tool_summary::{ToolSummary, summarize},
};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use saya_core::{
    contract::{MAX_MCP_STATUS_SERVERS, McpServerState, McpServerStatus, McpTransport},
    tools::todo::{TodoItem, TodoStatus},
};
use std::collections::{HashSet, VecDeque};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;
pub const MAX_TRANSCRIPT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_MESSAGES: usize = 512;
const MAX_RENDER_LINES: usize = 10_000;
const MAX_DETAIL_LINES: usize = 10_000;
const MAX_DETAIL_TITLE_BYTES: usize = 256;
const MAX_COMPLETION_ROWS: usize = 6;
const MAX_CONTEXT_ENTRIES: usize = 9;
const MAX_TODOS: usize = 64;
const WHEEL_LINES: usize = 3;
const MAX_FOOTNOTE_ANCHORS: usize = 2048;
const MAX_FOOTNOTE_HISTORY: usize = 64;

mod memory;
mod picker;
mod render_cache;
use render_cache::RenderCache;
mod selection;
pub(crate) use selection::MAX_COPY_BYTES;
use selection::Selection;
mod draw;
mod overlays;
pub use draw::draw;
use draw::safe_line;
use overlays::crop_display;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MarkdownDisplay {
    Source,
    #[default]
    Render,
}

impl MarkdownDisplay {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => command::MARKDOWN_SOURCE_VALUE,
            Self::Render => command::MARKDOWN_RENDER_VALUE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Tool,
    Thinking,
    System,
    RunSummary,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub id: u64,
    pub role: Role,
    pub label: String,
    pub text: String,
    pub streaming: bool,
    pub truncated: bool,
    pub(crate) tool_summary: Option<ToolSummary>,
    pub(crate) tool_card: bool,
}

#[derive(Debug, Clone)]
pub struct Details {
    pub title: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Default)]
pub struct MemoryPanel {
    pub lines: Vec<String>,
    pub input: Editor,
    pub settings: String,
    pub scroll: usize,
    pub page_rows: usize,
    pub busy: bool,
    pub confirmation: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Skills,
    Mcps,
    Plugins,
    Sessions,
    Goals,
}

#[derive(Debug, Clone)]
pub struct PickerItem {
    pub id: String,
    pub label: String,
    pub description: String,
    pub details: Vec<String>,
    pub active: bool,
}

#[derive(Debug)]
pub struct PickerPanel {
    pub kind: PickerKind,
    pub title: String,
    pub items: Vec<PickerItem>,
    pub selected: usize,
    pub scroll: usize,
    pub page_rows: usize,
    pub hint: String,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct FootnoteLocation {
    message: u64,
    key: String,
    definition: bool,
    occurrence: usize,
}

#[derive(Debug)]
struct FootnotePosition {
    location: FootnoteLocation,
    number: usize,
    line: usize,
    column: usize,
    width: usize,
}

#[derive(Debug)]
struct FootnoteHit {
    area: Rect,
    location: FootnoteLocation,
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
    pub activity_active: bool,
    pub activity_frame: u64,
    pub copied_notice: bool,
    pub pending_mode: Option<String>,
    pub usage: String,
    pub context_window_tokens: Option<u64>,
    pub context_input_tokens: Option<u64>,
    pub context_estimated: bool,
    pub context_breakdown: Vec<ContextEntry>,
    pub context_breakdown_estimated: bool,
    pub skills_count: Option<usize>,
    pub plugins_enabled: Option<bool>,
    pub thinking_effort: Option<String>,
    pub todos: Vec<TodoItem>,
    pub mcp_servers: Vec<McpServerStatus>,
    pub editor: Editor,
    pub completion: Completion,
    /// Selected catalog index while the command modal is open.
    pub command_palette: Option<usize>,
    pub memory_panel: Option<MemoryPanel>,
    pub picker_panel: Option<PickerPanel>,
    pub markdown_display: MarkdownDisplay,
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
    footnote_positions: VecDeque<FootnotePosition>,
    footnote_hits: Vec<FootnoteHit>,
    pending_footnote: Option<FootnoteLocation>,
    footnote_history: VecDeque<FootnoteLocation>,
    render_cache: RenderCache,
    selection: Selection,
}

impl Default for State {
    fn default() -> Self {
        Self {
            session_title: common::NEW_SESSION.into(),
            workspace: ".".into(),
            profile: common::UNCONFIGURED.into(),
            model: String::new(),
            mode: mode_text::display("Work"),
            permissions: permission_text::AUTO.into(),
            status: common::READY.into(),
            activity_active: false,
            activity_frame: 0,
            copied_notice: false,
            pending_mode: None,
            usage: String::new(),
            context_window_tokens: None,
            context_input_tokens: None,
            context_estimated: false,
            context_breakdown: Vec::new(),
            context_breakdown_estimated: true,
            skills_count: None,
            plugins_enabled: None,
            thinking_effort: None,
            todos: Vec::new(),
            mcp_servers: Vec::new(),
            editor: Editor::default(),
            completion: Completion::default(),
            command_palette: None,
            memory_panel: None,
            picker_panel: None,
            markdown_display: MarkdownDisplay::default(),
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
            footnote_positions: VecDeque::new(),
            footnote_hits: Vec::new(),
            pending_footnote: None,
            footnote_history: VecDeque::new(),
            render_cache: RenderCache::default(),
            selection: Selection::default(),
        }
    }
}

impl State {
    /// Handle presentation-only mouse gestures, including selection and clicks.
    pub fn mouse(&mut self, event: MouseEvent) -> bool {
        if let Some(panel) = &mut self.picker_panel {
            match event.kind {
                MouseEventKind::ScrollUp => panel.selected = panel.selected.saturating_sub(1),
                MouseEventKind::ScrollDown if !panel.items.is_empty() => {
                    panel.selected = panel.selected.saturating_add(1).min(panel.items.len() - 1)
                }
                _ => return false,
            }
            return true;
        }
        if let Some(panel) = &mut self.memory_panel {
            if contains(self.details_viewport, event.column, event.row) {
                match event.kind {
                    MouseEventKind::ScrollUp => {
                        panel.scroll = panel.scroll.saturating_sub(WHEEL_LINES)
                    }
                    MouseEventKind::ScrollDown => {
                        panel.scroll = panel.scroll.saturating_add(WHEEL_LINES)
                    }
                    _ => return false,
                }
                return true;
            }
            return false;
        }
        if self.command_palette.is_some() {
            self.selection.reset();
            let selected = self.palette_index_at(event.column, event.row);
            return match event.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(selected) = selected {
                        self.command_palette = Some(selected);
                        true
                    } else {
                        false
                    }
                }
                MouseEventKind::ScrollUp if selected.is_some() => self.palette_scroll(true),
                MouseEventKind::ScrollDown if selected.is_some() => self.palette_scroll(false),
                _ => false,
            };
        }
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection.begin(event.column, event.row);
                false
            }
            MouseEventKind::Drag(MouseButton::Left) => self.selection.drag(event.column, event.row),
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(point) = self.selection.release(event.column, event.row) {
                    self.footnote_at(point.x, point.y) || self.toggle_at(point.x, point.y)
                } else {
                    self.selection.copy();
                    true
                }
            }
            MouseEventKind::ScrollUp => self.scroll_at(event.column, event.row, true),
            MouseEventKind::ScrollDown => self.scroll_at(event.column, event.row, false),
            _ => false,
        }
    }

    /// Clear any pending or highlighted mouse selection.
    pub fn clear_text_selection(&mut self) -> bool {
        self.selection.clear()
    }

    /// Queue a clipboard request for selected visible text. No terminal I/O occurs here.
    pub fn copy_text_selection(&mut self) -> bool {
        if self.copied_notice {
            return true;
        }
        if self.command_palette.is_some()
            || self.picker_panel.is_some()
            || self.completion_viewport.is_some()
        {
            self.selection.reset();
            return false;
        }
        self.selection.copy()
    }

    pub(crate) fn take_clipboard_request(&mut self) -> Option<String> {
        self.selection.take_clipboard()
    }

    pub fn activity_visible(&self) -> bool {
        self.activity_active
            && matches!(
                self.status.as_str(),
                common::READY
                    | common::RUNNING
                    | common::THINKING
                    | common::RESPONDING
                    | common::REVIEWING
                    | common::COMPACTING
                    | common::CANCELLING
            )
    }

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
            tool_summary: None,
            tool_card: false,
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
        self.render_cache.remove_message(id);
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
        message.tool_summary = None;
        self.render_cache.remove_message(id);
        self.enforce_limits();
        true
    }

    /// Reset the viewport for a session switch while keeping message IDs distinct.
    pub fn clear_messages(&mut self) {
        self.selection.reset();
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
        self.picker_panel = None;
        self.palette_hits.clear();
        self.palette_first = 0;
        self.fold_anchor = None;
        self.clear_footnote_navigation();
        self.render_cache.clear();
    }

    /// Select the assistant presentation for this UI and discard stale hit targets.
    pub fn set_markdown_display(&mut self, display: MarkdownDisplay) {
        self.selection.reset();
        if self.markdown_display != display {
            self.markdown_display = display;
            self.last_width = 0;
            self.last_line_count = 0;
            self.clear_footnote_navigation();
            self.render_cache.clear();
        }
    }

    fn clear_footnote_navigation(&mut self) {
        self.footnote_positions.clear();
        self.footnote_hits.clear();
        self.pending_footnote = None;
        self.footnote_history.clear();
    }

    fn render_candidates(&self) -> HashSet<u64> {
        self.messages
            .iter()
            .rev()
            .filter(|message| message.role == Role::Assistant)
            .take(render_cache::MAX_ENTRIES)
            .map(|message| message.id)
            .collect()
    }

    /// Resolve a numbered note in the newest retained assistant document.
    pub fn jump_footnote(&mut self, number: usize) -> bool {
        if self.markdown_display != MarkdownDisplay::Render || number == 0 {
            return false;
        }
        let Some(target) = self
            .footnote_positions
            .iter()
            .rev()
            .find(|position| position.location.definition && position.number == number)
        else {
            return false;
        };
        let target = target.location.clone();
        if let Some(reference) = self.footnote_positions.iter().find(|position| {
            position.location.message == target.message
                && position.location.key == target.key
                && !position.location.definition
        }) {
            self.footnote_history.push_back(reference.location.clone());
            while self.footnote_history.len() > MAX_FOOTNOTE_HISTORY {
                self.footnote_history.pop_front();
            }
        }
        self.selection.clear();
        self.pending_footnote = Some(target);
        true
    }

    /// Return to the most recent retained footnote reference.
    pub fn footnote_back(&mut self) -> bool {
        if self.markdown_display != MarkdownDisplay::Render {
            return false;
        }
        while let Some(reference) = self.footnote_history.pop_back() {
            if self
                .footnote_positions
                .iter()
                .any(|position| position.location == reference)
            {
                self.selection.clear();
                self.pending_footnote = Some(reference);
                return true;
            }
        }
        false
    }

    /// Follow only a semantic note marker drawn in the latest transcript frame.
    pub fn footnote_at(&mut self, x: u16, y: u16) -> bool {
        if self.markdown_display != MarkdownDisplay::Render
            || self.command_palette.is_some()
            || contains(self.completion_viewport, x, y)
        {
            return false;
        }
        let Some(hit) = self
            .footnote_hits
            .iter()
            .find(|hit| hit.area.contains((x, y).into()))
        else {
            return false;
        };
        let clicked = hit.location.clone();
        if clicked.definition {
            if let Some(index) = self.footnote_history.iter().rposition(|reference| {
                reference.message == clicked.message
                    && reference.key == clicked.key
                    && self
                        .footnote_positions
                        .iter()
                        .any(|position| &position.location == reference)
            }) {
                self.pending_footnote = self.footnote_history.remove(index);
                return true;
            }
            self.pending_footnote = self
                .footnote_positions
                .iter()
                .find(|position| {
                    position.location.message == clicked.message
                        && position.location.key == clicked.key
                        && !position.location.definition
                })
                .map(|position| position.location.clone());
            return self.pending_footnote.is_some();
        }
        let target = self
            .footnote_positions
            .iter()
            .find(|position| {
                position.location.message == clicked.message
                    && position.location.key == clicked.key
                    && position.location.definition
            })
            .map(|position| position.location.clone());
        if let Some(target) = target {
            self.footnote_history.push_back(clicked);
            while self.footnote_history.len() > MAX_FOOTNOTE_HISTORY {
                self.footnote_history.pop_front();
            }
            self.selection.clear();
            self.pending_footnote = Some(target);
            true
        } else {
            false
        }
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
        self.selection.clear();
        self.fold_anchor = None;
        self.pending_footnote = None;
        self.scroll = self.scroll.saturating_add(lines);
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.selection.clear();
        self.fold_anchor = None;
        self.pending_footnote = None;
        self.scroll = self.scroll.saturating_sub(lines);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.selection.clear();
        self.fold_anchor = None;
        self.pending_footnote = None;
        self.scroll = 0;
    }

    /// Toggle a foldable message only when its actual visible header was clicked.
    /// Coordinates are terminal cells from the most recent render.
    pub fn toggle_at(&mut self, x: u16, y: u16) -> bool {
        self.selection.clear();
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
        self.selection.clear();
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
            self.selection.clear();
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
        self.render_cache.retain(&self.render_candidates());
        self.todos.truncate(MAX_TODOS);
    }
}

fn contains(area: Option<Rect>, x: u16, y: u16) -> bool {
    area.is_some_and(|area| area.contains((x, y).into()))
}

fn foldable(message: &Message) -> bool {
    message.role == Role::Thinking
        || (message.role == Role::Tool
            && (message.tool_card
                || message.text.len() > 512
                || message.text.split('\n').take(7).count() > 6
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
            || retained + ui_text::DETAIL_TRUNCATION_NOTICE.len() > MAX_TRANSCRIPT_BYTES
        {
            let Some(tail) = bounded.pop() else { break };
            retained -= tail.len();
            let remaining = MAX_TRANSCRIPT_BYTES
                .saturating_sub(retained + ui_text::DETAIL_TRUNCATION_NOTICE.len());
            if bounded.len() < MAX_DETAIL_LINES - 1 && remaining > 0 {
                let end = prefix_boundary(&tail, remaining.min(tail.len()));
                retained += end;
                bounded.push(tail[..end].to_owned());
            }
        }
        bounded.push(ui_text::DETAIL_TRUNCATION_NOTICE.to_owned());
    }
    bounded.shrink_to_fit();
    details.lines = bounded;
}
