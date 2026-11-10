//! Live message identity and interface-only thinking snapshots.
use super::*;
use crate::tool_summary::{ToolSummary, summarize, summarize_call};
use crate::{
    attachments,
    i18n::{common, messages as text, tool as tool_text, view as ui_text},
};
use saya_app::thinking::{ThinkingLog, extract_reasoning};
use saya_core::{
    contract::ToolResult,
    tools::todo::{TodoItem, current_todos, result_todos},
};
use serde_json::Value;
use std::{collections::HashSet, time::Instant};

pub(super) struct ToolCard {
    pub id: u64,
    pub name: String,
    args: Option<Value>,
    started: Instant,
    pub finished: bool,
}

impl App {
    pub(super) fn event(&mut self, event: Event) {
        match event.payload {
            EventPayload::ModelStarted {
                request_id,
                purpose,
                ..
            } => {
                if purpose == ModelPurpose::Execution {
                    self.record_context_input(None);
                    if let Some(active) = &mut self.active {
                        if let Some(id) = active.assistant_id.take() {
                            self.state.finish_message(id);
                        }
                        active.execution_request = Some(request_id.clone());
                        let id = self.state.push_message(
                            Role::Thinking,
                            text::WAITING_PROVIDER,
                            text::WAITING_PROVIDER_TEXT,
                        );
                        self.state.append_message(id, "");
                        active.thinking_ids.insert(request_id.clone(), id);
                    }
                    self.sync_thinking_request(&request_id);
                }
                if self.pending.is_none() {
                    self.state.status = match purpose {
                        ModelPurpose::Review => common::REVIEWING,
                        ModelPurpose::Compaction => common::COMPACTING,
                        ModelPurpose::Execution => common::THINKING,
                    }
                    .into();
                }
            }
            EventPayload::ModelTextDelta { request_id, text } => {
                if self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.execution_request.as_deref() == Some(&request_id))
                {
                    let text = self.safe(&text);
                    if !text.is_empty() {
                        let id = self.assistant_message();
                        self.state.append_message(id, &text);
                        if self.pending.is_none() {
                            self.state.status = common::RESPONDING.into();
                        }
                    }
                }
            }
            EventPayload::ModelFinished {
                request_id,
                purpose: ModelPurpose::Execution,
                usage,
                ..
            } => {
                self.record_context_input(usage);
                self.sync_thinking_request(&request_id);
                if let Some(id) = self
                    .active
                    .as_ref()
                    .and_then(|active| active.thinking_ids.get(&request_id))
                    .copied()
                {
                    self.end_thinking(id);
                }
                if let Some(active) = &mut self.active
                    && active.execution_request.as_deref() == Some(&request_id)
                {
                    active.execution_request = None;
                    if let Some(id) = active.assistant_id {
                        self.state.finish_message(id);
                    }
                }
            }
            EventPayload::ToolRequested {
                call_id,
                name,
                args,
            } => {
                if let Some(id) = self.ensure_tool_card(&name, &call_id) {
                    let summary = self.safe_tool_summary(summarize_call(&name, &args, None, None));
                    if let Some(card) = self
                        .active
                        .as_mut()
                        .and_then(|active| active.tool_cards.get_mut(&call_id))
                    {
                        card.args = Some(args.clone());
                    }
                    let text = self.safe(&crate::tool_render::call(&name, &args, None));
                    self.state.replace_message(id, &text);
                    if let Some(message) = self
                        .state
                        .messages
                        .iter_mut()
                        .find(|message| message.id == id)
                    {
                        message.tool_summary = summary;
                        message.tool_card = true;
                    }
                }
            }
            EventPayload::ToolStarted { name, call_id } => {
                let label = self.safe(&format!("{name} · {call_id} · {}", ui_text::RUNNING));
                if let Some(id) = self.ensure_tool_card(&name, &call_id) {
                    let args = self
                        .active
                        .as_ref()
                        .and_then(|active| active.tool_cards.get(&call_id))
                        .and_then(|card| card.args.clone());
                    let running = args.as_ref().map_or_else(
                        || text::TOOL_RUNNING.to_owned(),
                        |args| self.safe(&crate::tool_render::call(&name, args, None)),
                    );
                    let summary = self.safe_tool_summary(
                        args.as_ref()
                            .and_then(|args| summarize_call(&name, args, None, None)),
                    );
                    self.state.replace_message(id, &running);
                    if let Some(message) = self
                        .state
                        .messages
                        .iter_mut()
                        .find(|message| message.id == id)
                    {
                        message.tool_summary = summary;
                    }
                    if let Some(card) = self
                        .active
                        .as_mut()
                        .and_then(|active| active.tool_cards.get_mut(&call_id))
                    {
                        card.name = name;
                    }
                    if let Some(message) = self
                        .state
                        .messages
                        .iter_mut()
                        .find(|message| message.id == id)
                    {
                        message.label = label;
                    }
                }
                if self.pending.is_none() {
                    self.state.status = common::RUNNING.into();
                }
            }
            EventPayload::McpStatus { servers } => {
                self.install_mcp_statuses(servers);
            }
            EventPayload::ToolFinished {
                name,
                call_id,
                result,
            } => {
                self.tool_finished(name, call_id, result);
            }
            _ => {}
        }
        self.prune_message_ids();
    }

    fn install_mcp_statuses(&mut self, servers: Vec<saya_core::contract::McpServerStatus>) {
        use saya_core::{
            contract::{MAX_MCP_STATUS_SERVERS, McpTransport},
            mcp::config::McpTransportSettings,
        };
        for mut status in servers.into_iter().take(MAX_MCP_STATUS_SERVERS) {
            let known = self
                .config
                .mcp
                .servers
                .get(&status.server)
                .is_some_and(|server| {
                    server.enabled
                        && matches!(
                            (&server.transport, status.transport),
                            (McpTransportSettings::Stdio(_), McpTransport::Stdio)
                                | (McpTransportSettings::Http(_), McpTransport::Http)
                        )
                });
            if !known {
                continue;
            }
            status.server = self.safe(&status.server);
            if let Some(existing) = self
                .state
                .mcp_servers
                .iter_mut()
                .find(|existing| existing.server == status.server)
            {
                *existing = status;
            }
        }
    }

    fn ensure_tool_card(&mut self, name: &str, call_id: &str) -> Option<u64> {
        if let Some(id) = self
            .active
            .as_ref()
            .and_then(|active| active.tool_cards.get(call_id))
            .map(|card| card.id)
        {
            return Some(id);
        }
        self.active.as_ref()?;
        let label = self.safe(&format!("{name} · {call_id} · {}", ui_text::RUNNING));
        let id = self
            .state
            .push_message(Role::Tool, label, text::TOOL_RUNNING);
        if let Some(message) = self
            .state
            .messages
            .iter_mut()
            .find(|message| message.id == id)
        {
            message.tool_card = true;
        }
        if let Some(active) = &mut self.active {
            active.tool_cards.insert(
                call_id.to_owned(),
                ToolCard {
                    id,
                    name: name.to_owned(),
                    args: None,
                    started: Instant::now(),
                    finished: false,
                },
            );
        }
        Some(id)
    }

    fn safe_tool_summary(&self, summary: Option<ToolSummary>) -> Option<ToolSummary> {
        summary.map(|mut summary| {
            summary.text = self.safe(&summary.text);
            summary
        })
    }

    pub(super) fn assistant_message(&mut self) -> u64 {
        if let Some(id) = self
            .active
            .as_ref()
            .and_then(|active| active.assistant_id)
            .filter(|id| self.state.messages.iter().any(|message| message.id == *id))
        {
            return id;
        }
        let id = self
            .state
            .push_message(Role::Assistant, self.state.model.clone(), "");
        if let Some(active) = &mut self.active {
            active.assistant_id = Some(id);
        }
        id
    }

    fn tool_finished(&mut self, name: String, call_id: String, result: ToolResult) {
        let card = self
            .active
            .as_ref()
            .and_then(|active| active.tool_cards.get(&call_id))
            .map(|card| {
                (
                    card.id,
                    card.args.clone(),
                    card.started.elapsed(),
                    card.finished,
                )
            });
        if card.as_ref().is_some_and(|(_, _, _, finished)| *finished) {
            return;
        }
        if name == "todo_write"
            && let Some(todos) = result_todos(&result)
        {
            self.install_todos(todos);
        }
        let label = self.safe(&format!(
            "{name} · {call_id} · {}",
            if result.ok {
                tool_text::COMPLETED
            } else {
                tool_text::FAILED
            }
        ));
        let args = card.as_ref().and_then(|(_, args, _, _)| args.as_ref());
        let elapsed = card.as_ref().map(|(_, _, elapsed, _)| *elapsed);
        let tool_summary = self.safe_tool_summary(
            args.and_then(|args| summarize_call(&name, args, Some(&result), elapsed))
                .or_else(|| {
                    serde_json::to_string(&result)
                        .ok()
                        .as_deref()
                        .and_then(summarize)
                }),
        );
        let text = self.safe(&args.map_or_else(
            || crate::tool_render::result(&result),
            |args| crate::tool_render::call_and_result(&name, args, &result, elapsed),
        ));
        let existing = card
            .as_ref()
            .map(|(id, _, _, _)| *id)
            .filter(|id| self.state.messages.iter().any(|message| message.id == *id));
        let id = if let Some(id) = existing {
            self.state.replace_message(id, &text);
            if let Some(message) = self
                .state
                .messages
                .iter_mut()
                .find(|message| message.id == id)
            {
                message.label = label;
                message.truncated |= result.truncated;
                message.tool_summary = tool_summary;
                message.tool_card = true;
            }
            id
        } else {
            let id = self.state.push_message(Role::Tool, label, text);
            if let Some(message) = self
                .state
                .messages
                .iter_mut()
                .find(|message| message.id == id)
            {
                message.truncated |= result.truncated;
                message.tool_summary = tool_summary;
                message.tool_card = true;
            }
            id
        };
        if let Some(active) = &mut self.active {
            if let Some(card) = active.tool_cards.get_mut(&call_id) {
                card.finished = true;
                card.name = name;
            } else {
                active.tool_cards.insert(
                    call_id,
                    ToolCard {
                        id,
                        name,
                        args: None,
                        started: Instant::now(),
                        finished: true,
                    },
                );
            }
        }
    }

    pub(super) fn install_todos(&mut self, mut todos: Vec<TodoItem>) {
        for todo in &mut todos {
            todo.id = self.safe(&todo.id);
            todo.content = self.safe(&todo.content);
        }
        self.state.todos = todos;
    }

    pub(super) fn refresh_thinking(&mut self) -> bool {
        if !self
            .active
            .as_ref()
            .is_some_and(|active| active.thinking.has_changed().unwrap_or(false))
        {
            return false;
        }
        self.sync_thinking();
        true
    }

    pub(super) fn sync_thinking(&mut self) {
        let Some(active) = &mut self.active else {
            return;
        };
        let log = active.thinking.borrow_and_update().clone();
        self.record_thinking(log);
    }

    fn sync_thinking_request(&mut self, request_id: &str) {
        let entry = self.active.as_ref().and_then(|active| {
            active
                .thinking
                .borrow()
                .entries
                .iter()
                .find(|entry| self.safe(&entry.request_id) == request_id)
                .cloned()
        });
        if let Some(entry) = entry {
            self.record_thinking(ThinkingLog {
                entries: vec![entry],
            });
        }
    }

    fn record_thinking(&mut self, log: ThinkingLog) {
        for entry in log.entries {
            let request_id = self.safe(&entry.request_id);
            let Some(id) = self
                .active
                .as_ref()
                .and_then(|active| active.thinking_ids.get(&request_id))
                .copied()
            else {
                continue;
            };
            let label = if entry.available {
                text::PROVIDER_THOUGHT
            } else if entry.active {
                text::WAITING_PROVIDER
            } else {
                text::UNAVAILABLE
            };
            let text = if !entry.text.is_empty() {
                self.safe(&entry.text)
            } else if entry.truncated {
                text::THINKING_LIMIT.into()
            } else if entry.available && entry.active {
                text::RECEIVING_THOUGHT.into()
            } else if entry.active {
                text::WAITING_PROVIDER_TEXT.into()
            } else {
                text::THINKING_NOT_RETURNED.into()
            };
            self.state.replace_message(id, text);
            if let Some(message) = self
                .state
                .messages
                .iter_mut()
                .find(|message| message.id == id)
            {
                message.label = label.into();
                message.streaming = entry.active;
                message.truncated |= entry.truncated;
            }
        }
    }

    fn prune_message_ids(&mut self) {
        let ids = self
            .state
            .messages
            .iter()
            .map(|message| message.id)
            .collect::<HashSet<_>>();
        if let Some(active) = &mut self.active {
            active.thinking_ids.retain(|_, id| ids.contains(id));
            active.tool_cards.retain(|_, card| ids.contains(&card.id));
        }
    }

    pub(super) fn close_run_messages(&mut self, active: &Active) {
        if let Some(id) = active.assistant_id {
            self.state.finish_message(id);
        }
        for id in active.thinking_ids.values() {
            self.end_thinking(*id);
        }
        for (call_id, card) in &active.tool_cards {
            if !card.finished {
                let text = card.args.as_ref().map_or_else(
                    || text::TOOL_ENDED_UNKNOWN.to_owned(),
                    |args| {
                        self.safe(&format!(
                            "{}\n\n{}",
                            crate::tool_render::call(
                                &card.name,
                                args,
                                Some(card.started.elapsed())
                            ),
                            text::TOOL_ENDED_UNKNOWN
                        ))
                    },
                );
                self.state.replace_message(card.id, text);
                let label = self.safe(&format!(
                    "{} · {call_id} · {}",
                    card.name,
                    tool_text::UNKNOWN
                ));
                if let Some(message) = self
                    .state
                    .messages
                    .iter_mut()
                    .find(|message| message.id == card.id)
                {
                    message.label = label;
                    message.tool_summary = None;
                }
            }
        }
    }

    fn end_thinking(&mut self, id: u64) {
        if let Some(message) = self
            .state
            .messages
            .iter_mut()
            .find(|message| message.id == id)
        {
            if message.label.starts_with(text::WAITING_PROVIDER)
                || message.text == text::RECEIVING_THOUGHT
            {
                message.label = text::UNAVAILABLE.into();
                message.text = text::THINKING_UNAVAILABLE_LOG.into();
            } else if message.streaming && message.label == text::PROVIDER_THOUGHT {
                message.label = text::PARTIAL_THOUGHT.into();
                message.truncated = true;
            }
            message.streaming = false;
        }
    }

    pub(super) fn restore_messages(&mut self, history: &[HistoryItem]) {
        self.install_todos(current_todos(history));
        let visible_start = history.len().saturating_sub(512);
        let mut index = 0;
        let mut tools = HashMap::new();
        while index < history.len() {
            if matches!(
                &history[index],
                HistoryItem::UserText(_) | HistoryItem::ToolResult { .. }
            ) {
                if index >= visible_start {
                    self.restore_item(&history[index], &mut tools);
                }
                index += 1;
                continue;
            }
            let end = history[index..]
                .iter()
                .position(|item| {
                    matches!(
                        item,
                        HistoryItem::UserText(_) | HistoryItem::ToolResult { .. }
                    )
                })
                .map_or(history.len(), |offset| index + offset);
            if end > visible_start {
                let thought = extract_reasoning(&history[index..end]);
                if !thought.is_empty() {
                    self.state.push_message(
                        Role::Thinking,
                        text::STORED_THOUGHT,
                        self.safe(&thought),
                    );
                }
                for item in &history[index.max(visible_start)..end] {
                    self.restore_item(item, &mut tools);
                }
            }
            index = end;
        }
    }

    fn restore_item(
        &mut self,
        item: &HistoryItem,
        tools: &mut HashMap<String, (u64, String, Value)>,
    ) {
        match item {
            HistoryItem::UserText(text) => {
                self.state.push_message(
                    Role::User,
                    ui_text::YOU,
                    self.safe(&attachments::display(text)),
                );
            }
            HistoryItem::AssistantText(text) => {
                if !text.is_empty() {
                    self.state.push_message(
                        Role::Assistant,
                        self.state.model.clone(),
                        self.safe(text),
                    );
                }
            }
            HistoryItem::AssistantPartial(text) => {
                if !text.is_empty() {
                    self.state.push_message(
                        Role::Assistant,
                        text::INTERRUPTED_PARTIAL,
                        self.safe(text),
                    );
                }
            }
            HistoryItem::ToolCall(call) => {
                let label = self.safe(&format!(
                    "{} · {} · {}",
                    call.name,
                    call.call_id,
                    text::STORED_REQUEST
                ));
                let arguments = serde_json::from_str::<Value>(&call.arguments)
                    .unwrap_or_else(|_| Value::String(call.arguments.clone()));
                let text = self.safe(&format!(
                    "{}\n\n{}",
                    crate::tool_render::call(&call.name, &arguments, None),
                    text::stored_request(&call.call_id)
                ));
                let id = self.state.push_message(Role::Tool, label, text);
                if let Some(message) = self
                    .state
                    .messages
                    .iter_mut()
                    .find(|message| message.id == id)
                {
                    message.tool_card = true;
                }
                tools.insert(call.call_id.clone(), (id, call.name.clone(), arguments));
            }
            HistoryItem::ToolResult { call_id, result } => {
                let status = if result.ok {
                    text::STORED_COMPLETED
                } else {
                    text::STORED_FAILED
                };
                if let Some((id, name, args)) = tools.remove(call_id) {
                    let text = self.safe(&crate::tool_render::call_and_result(
                        &name, &args, result, None,
                    ));
                    let tool_summary =
                        self.safe_tool_summary(summarize_call(&name, &args, Some(result), None));
                    self.state.replace_message(id, &text);
                    let label = self.safe(&format!("{name} · {call_id} · {status}"));
                    if let Some(message) = self
                        .state
                        .messages
                        .iter_mut()
                        .find(|message| message.id == id)
                    {
                        message.label = label;
                        message.truncated |= result.truncated;
                        message.tool_summary = tool_summary;
                        message.tool_card = true;
                    }
                } else {
                    let text = self.safe(&crate::tool_render::result(result));
                    let tool_summary = self.safe_tool_summary(
                        serde_json::to_string(result)
                            .ok()
                            .as_deref()
                            .and_then(summarize),
                    );
                    let id = self.state.push_message(
                        Role::Tool,
                        self.safe(&format!("{} · {call_id} · {status}", text::STORED_RESULT)),
                        text,
                    );
                    if let Some(message) = self
                        .state
                        .messages
                        .iter_mut()
                        .find(|message| message.id == id)
                    {
                        message.truncated |= result.truncated;
                        message.tool_summary = tool_summary;
                        message.tool_card = true;
                    }
                }
            }
            HistoryItem::ProviderItem { .. } => {}
        }
    }
}
