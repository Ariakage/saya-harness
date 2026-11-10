//! Explicit user administration, kept separate from model tool permissions.
use super::*;
use crate::{i18n::memory as text, view::MemoryPanel};
use saya_app::memory_admin::{MemoryAdmin, PreparedMemoryAction};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) enum MemoryUiResult {
    View { data: Value, next: Option<String> },
    Review(PreparedMemoryAction),
    Publication(Value),
}
enum Task {
    Query(String),
    Prepare(String),
    Confirm(PreparedMemoryAction),
}

fn failure(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        saya_core::contract::ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}
fn continuation(command: &str, data: &Value) -> Option<String> {
    let (verb, argument) = command
        .trim()
        .split_once(' ')
        .unwrap_or((command.trim(), ""));
    let data = &data["data"];
    match verb {
        "" | "list" | "history" => data["next_page"].as_u64().map(|page| {
            format!(
                "{} {page}",
                if verb == "history" { "history" } else { "list" }
            )
        }),
        "search" | "show" if !data["next_cursor"].is_null() => {
            let mut options = if argument.trim().starts_with('{') {
                serde_json::from_str::<Value>(argument).ok()?
            } else if verb == "search" {
                json!({"query":argument})
            } else if argument.starts_with("fact_") {
                json!({"fact_id":argument})
            } else {
                json!({"entry_id":argument})
            };
            options
                .as_object_mut()?
                .insert("cursor".into(), data["next_cursor"].clone());
            Some(format!("{verb} {options}"))
        }
        _ => None,
    }
}

impl App {
    fn memory_settings(&self) -> String {
        text::settings(
            self.config.memory.read,
            self.config.memory.inject,
            self.config.memory.write,
            &self.state.mode,
        )
    }
    fn memory_lines(&mut self, lines: Vec<String>) {
        let lines = lines.into_iter().map(|line| self.safe(&line)).collect();
        let settings = self.safe(&self.memory_settings());
        if let Some(panel) = &mut self.state.memory_panel {
            panel.lines = lines;
            panel.scroll = 0;
            panel.settings = settings;
        }
    }
    pub(super) fn scrub_memory(&mut self) -> bool {
        let Some(panel) = &self.state.memory_panel else {
            return false;
        };
        let safe = panel
            .lines
            .iter()
            .map(|line| self.safe(line))
            .collect::<Vec<_>>();
        let changed = safe != panel.lines;
        if changed && let Some(panel) = &mut self.state.memory_panel {
            panel.lines = safe;
        }
        changed
    }
    pub(super) async fn open_memory(&mut self, argument: &str) -> Result<()> {
        if self.pending.is_some() {
            return Err(text::PENDING_REQUEST.into());
        }
        if self.ui_job.is_some() {
            return Err(text::UI_ACTIVE_WAIT.into());
        }
        self.memory_pending = None;
        self.memory_next = None;
        self.state.command_palette = None;
        self.state.completion.dismiss();
        self.state.memory_panel = Some(MemoryPanel {
            settings: self.memory_settings(),
            ..Default::default()
        });
        if !self.config.memory.read {
            self.memory_lines(vec![
                text::READING_DISABLED.into(),
                text::ESC_DRAFT_HINT.into(),
            ]);
        } else {
            self.memory_command(if argument.is_empty() {
                "list"
            } else {
                argument
            })
            .await?;
        }
        Ok(())
    }
    fn memory_worker(&mut self, task: Task) -> Result<()> {
        if self.ui_job.is_some() {
            return Err(text::UI_ACTIVE_CANCEL.into());
        }
        let config = self.config.clone();
        let secrets = self.secret_registry.clone();
        let cancel = Arc::new(RunCancellation::new());
        let checking = cancel.clone();
        let flag = Arc::new(AtomicBool::new(false));
        let stopping = flag.clone();
        let mut worker = tokio::task::spawn_blocking(move || {
            let admin = MemoryAdmin::open(&config, secrets)?;
            let result = match task {
                Task::Query(command) => {
                    let data = admin.query(&command, &flag)?;
                    let next = continuation(&command, &data);
                    MemoryUiResult::View { data, next }
                }
                Task::Prepare(command) => MemoryUiResult::Review(admin.prepare(&command, &flag)?),
                Task::Confirm(prepared) => {
                    MemoryUiResult::Publication(admin.confirm(prepared, &flag)?)
                }
            };
            Ok(services::UiResult::Memory(result))
        });
        let join = tokio::spawn(async move {
            let result = tokio::select! {
                result=&mut worker=>result,
                _=checking.cancelled()=>{stopping.store(true,Ordering::SeqCst);worker.await},
            };
            result.map_err(|_| failure("ui_worker_failed", text::WORKER_FAILED))?
        });
        self.begin_ui(cancel, join);
        if let Some(panel) = &mut self.state.memory_panel {
            panel.busy = true;
        }
        Ok(())
    }
    pub(super) async fn memory_command(&mut self, command: &str) -> Result<()> {
        if command.len() > saya_app::memory_admin::MAX_VIEW_BYTES {
            return Err(text::COMMAND_TOO_LONG.into());
        }
        let command = command.trim();
        let (verb, argument) = command.split_once(' ').unwrap_or((command, ""));
        if matches!(verb, "close" | "exit") {
            if let Some(job) = &self.ui_job {
                job.cancel.cancel();
            }
            self.memory_pending = None;
            self.memory_next = None;
            self.state.memory_panel = None;
            return Ok(());
        }
        if self.ui_job.is_some() {
            return Err(text::OPERATION_ACTIVE.into());
        }
        match verb {
            "help" => self.memory_lines(
                text::HELP_LINES
                    .iter()
                    .copied()
                    .map(str::to_owned)
                    .collect(),
            ),
            "set" => {
                if self.active.is_some() {
                    return Err(text::IDLE_SETTINGS.into());
                }
                let (setting, value) = argument.split_once(' ').ok_or(text::SET_USAGE)?;
                let value = match value.trim() {
                    "on" => true,
                    "off" => false,
                    _ => return Err(text::ON_OFF_USAGE.into()),
                };
                match setting {
                    "read" => self.config.memory.read = value,
                    "inject" => self.config.memory.inject = value,
                    "write" => self.config.memory.write = value,
                    _ => return Err(text::SET_USAGE.into()),
                };
                self.memory_pending = None;
                self.memory_next = None;
                if let Some(panel) = &mut self.state.memory_panel {
                    panel.confirmation = None;
                }
                self.memory_lines(vec![text::set_result(setting, value)]);
            }
            "reject" => {
                self.memory_pending = None;
                if let Some(panel) = &mut self.state.memory_panel {
                    panel.confirmation = None;
                }
                self.memory_lines(vec![text::PENDING_DISCARDED.into()]);
            }
            "review" if argument.is_empty() => {
                let data = self
                    .memory_pending
                    .as_ref()
                    .ok_or(text::PREPARE_BEFORE_REVIEW)?
                    .review()
                    .clone();
                self.memory_lines(json_lines(&data)?);
            }
            "confirm" => {
                if self.active.is_some() {
                    return Err(text::PUBLICATION_IDLE.into());
                }
                let prepared = self
                    .memory_pending
                    .as_ref()
                    .ok_or(text::PREPARE_BEFORE_CONFIRM)?;
                let expected = format!(
                    "{} {}",
                    prepared.operation_id(),
                    prepared.expected_revision()
                );
                if argument.trim() != expected {
                    return Err(text::CONFIRMATION_MISMATCH.into());
                }
                self.memory_last_operation = Some(prepared.operation_id().into());
                let prepared = self.memory_pending.take().ok_or(text::REVIEW_EXPIRED)?;
                if let Some(panel) = &mut self.state.memory_panel {
                    panel.confirmation = None;
                }
                self.memory_worker(Task::Confirm(prepared))?;
            }
            "correct" | "forget" | "diff" | "review" | "rollback" | "export" | "restore" => {
                if self.active.is_some() {
                    return Err(text::CHANGE_IDLE.into());
                }
                self.memory_pending = None;
                if let Some(panel) = &mut self.state.memory_panel {
                    panel.confirmation = None;
                }
                self.memory_worker(Task::Prepare(command.into()))?;
            }
            "next" => {
                let command = self.memory_next.clone().ok_or(text::NO_NEXT_PAGE)?;
                self.memory_worker(Task::Query(command))?;
            }
            "" | "list" | "search" | "show" | "sources" | "history" | "operation" => {
                self.memory_worker(Task::Query(command.into()))?
            }
            _ => return Err(text::UNKNOWN_COMMAND.into()),
        }
        Ok(())
    }
    pub(super) fn finish_memory(&mut self, result: MemoryUiResult, cancelled: bool) -> Result<()> {
        if let Some(panel) = &mut self.state.memory_panel {
            panel.busy = false;
        }
        match result {
            MemoryUiResult::View { data, next } if !cancelled => {
                self.memory_next = next;
                self.memory_lines(json_lines(&data)?);
            }
            MemoryUiResult::Review(prepared) if !cancelled && self.state.memory_panel.is_some() => {
                let mut lines = json_lines(prepared.review())?;
                let confirmation = format!(
                    "confirm {} {}",
                    prepared.operation_id(),
                    prepared.expected_revision()
                );
                lines.push(String::new());
                lines.push(confirmation.clone());
                self.memory_lines(lines);
                if let Some(panel) = &mut self.state.memory_panel {
                    panel.confirmation = Some(confirmation);
                }
                self.memory_pending = Some(prepared);
            }
            MemoryUiResult::Publication(data) => {
                self.memory_next = None;
                self.memory_pending = None;
                let lines = json_lines(&data)?;
                if self.state.memory_panel.is_some() {
                    self.memory_lines(lines);
                } else {
                    self.notice(&text::outcome(&self.safe(&data.to_string())));
                }
                if data["requires_recovery"] == true {
                    self.notice(text::PUBLICATION_UNKNOWN);
                } else if cancelled {
                    self.notice(text::FINISHED_DURING_CANCEL);
                }
            }
            _ => self.memory_lines(vec![text::QUERY_CANCELLED.into()]),
        }
        Ok(())
    }
    pub(super) async fn memory_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.kind != KeyEventKind::Press && matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => {
                    if let Some(job) = &self.ui_job {
                        job.cancel.cancel();
                    } else {
                        self.memory_pending = None;
                        if let Some(panel) = &mut self.state.memory_panel {
                            panel.confirmation = None;
                            panel.input.clear();
                        }
                    }
                    return Ok(false);
                }
                KeyCode::Char('d') => {
                    self.memory_command("close").await?;
                    return Ok(false);
                }
                KeyCode::Char('p') => return Ok(false),
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc => self.memory_command("close").await?,
            KeyCode::PageUp => {
                if let Some(panel) = &mut self.state.memory_panel {
                    panel.scroll = panel.scroll.saturating_sub(panel.page_rows);
                }
            }
            KeyCode::PageDown => {
                if let Some(panel) = &mut self.state.memory_panel {
                    panel.scroll = panel.scroll.saturating_add(panel.page_rows);
                }
            }
            KeyCode::Enter => {
                let command = self
                    .state
                    .memory_panel
                    .as_ref()
                    .map(|panel| panel.input.text().to_owned())
                    .unwrap_or_default();
                let result = self.memory_command(&command).await;
                if result.is_ok()
                    && let Some(panel) = &mut self.state.memory_panel
                {
                    panel.input.clear();
                }
                if let Err(error) = result {
                    let text = self.safe(&error.to_string());
                    if let Some(panel) = &mut self.state.memory_panel {
                        panel.lines.insert(0, crate::i18n::memory::error(&text));
                        panel.scroll = 0;
                        panel.lines.truncate(4096);
                    }
                }
            }
            _ => {
                if let Some(panel) = &mut self.state.memory_panel {
                    panel.input.handle_key(key);
                }
            }
        }
        Ok(false)
    }
}
fn json_lines(value: &Value) -> Result<Vec<String>> {
    let text = serde_json::to_string_pretty(value)?;
    if text.len() > 128 * 1024 {
        return Err(text::PRESENTATION_TOO_LONG.into());
    }
    Ok(text.lines().map(str::to_owned).collect())
}
