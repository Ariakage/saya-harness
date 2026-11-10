//! Debug-only fixtures and an interactive view without runtime preparation.
use crate::{
    commands::COMMANDS,
    i18n::{controller as controller_text, picker as picker_text},
    input::TerminalInput,
    terminal::TerminalGuard,
    tool_render,
    view::{
        self, ContextEntry, Details, MarkdownDisplay, MemoryPanel, PickerItem, PickerKind,
        PickerPanel, Role, State,
    },
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use saya_core::tools::todo::{TodoItem, TodoStatus};
use std::{io, time::Duration};
use tokio::signal::unix::{SignalKind, signal};

pub const SCENES: &[&str] = &[
    "chat",
    "approval",
    "attachments",
    "question",
    "background",
    "tool",
    "help",
    "commands",
    "completion",
    "memory",
    "skills",
    "mcps",
    "plugins",
    "sessions",
    "idle",
    "working",
    "completed",
    "compact",
    "effort",
    "sidebar",
    "polish",
    "markdown",
    "syntax",
    "syntax-blocks",
    "syntax-lists",
    "syntax-links",
    "footnotes",
    "math",
    "mermaid",
    "mermaid-sequence",
    "mermaid-state",
    "mermaid-class",
    "mermaid-er",
    "mermaid-fallback",
    "tasks",
    "folded",
    "expanded",
    "refined",
];

/// Open the fixture browser with no model, configuration, or storage preparation.
pub async fn run(scene: &str, source: bool) -> io::Result<u8> {
    let mut index = SCENES
        .iter()
        .position(|candidate| *candidate == scene)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unknown preview scene"))?;
    let display = if source {
        MarkdownDisplay::Source
    } else {
        MarkdownDisplay::Render
    };
    let mut state = scene_state(index, display);
    let mut terminal = TerminalGuard::enter()?;
    let mut input = TerminalInput::new()?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut activity = tokio::time::interval(Duration::from_millis(120));
    activity.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut exit = 0;
    loop {
        if let Some(text) = state.take_clipboard_request()
            && let Err(error) = terminal.copy_text(&text)
        {
            if error.kind() != io::ErrorKind::InvalidInput {
                return Err(error);
            }
            state.push_message(Role::System, "Clipboard", error.to_string());
        }
        terminal.set_title(&format!("Saya | TUI preview · {}", SCENES[index]))?;
        terminal.draw(|frame| view::draw(frame, &mut state))?;
        let event = tokio::select! {
            biased;
            _ = interrupt.recv() => { exit = 130; break; },
            _ = terminate.recv() => { exit = 130; break; },
            _ = hangup.recv() => { exit = 130; break; },
            event = input.next() => match event { Some(event) => event?, None => break },
            _ = activity.tick(), if state.activity_visible() => {
                state.activity_frame = state.activity_frame.wrapping_add(1);
                continue;
            },
        };
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    match key.code {
                        KeyCode::Char('c') => {
                            if key.kind != KeyEventKind::Press {
                                continue;
                            }
                            if state.copy_text_selection() {
                                continue;
                            }
                            exit = 130;
                            break;
                        }
                        KeyCode::Char('d') => break,
                        KeyCode::Char('o') => {
                            state.toggle_latest();
                        }
                        _ => {}
                    }
                    continue;
                }
                match key.code {
                    KeyCode::Esc if state.clear_text_selection() => {}
                    KeyCode::Esc | KeyCode::Char('q') => break,
                    KeyCode::Left | KeyCode::Right => {
                        index = if key.code == KeyCode::Right {
                            (index + 1) % SCENES.len()
                        } else {
                            (index + SCENES.len() - 1) % SCENES.len()
                        };
                        state = scene_state(index, state.markdown_display);
                    }
                    KeyCode::Char('s') => state.set_markdown_display(MarkdownDisplay::Source),
                    KeyCode::Char('r') => state.set_markdown_display(MarkdownDisplay::Render),
                    KeyCode::Char('b') => {
                        state.footnote_back();
                    }
                    KeyCode::Up => state.scroll_up(3),
                    KeyCode::Down => state.scroll_down(3),
                    KeyCode::PageUp => state.scroll_up(10),
                    KeyCode::PageDown => state.scroll_down(10),
                    KeyCode::Home => state.scroll_up(usize::MAX),
                    KeyCode::End => state.scroll_to_bottom(),
                    _ => {}
                }
            }
            Event::Mouse(event) => {
                state.mouse(event);
            }
            _ => {}
        }
    }
    drop(input);
    terminal.restore()?;
    Ok(exit)
}

fn scene_state(index: usize, display: MarkdownDisplay) -> State {
    let mut state = fixture(SCENES[index]);
    state.set_markdown_display(display);
    if !matches!(SCENES[index], "working" | "completed") {
        state.status = format!(
            "Preview {}/{} · {} · ←/→ scenes · s/r source/render · q exit",
            index + 1,
            SCENES.len(),
            SCENES[index]
        );
    }
    state
}

pub fn fixture(scene: &str) -> State {
    let mut state = State::default();
    state.push_message(
        Role::User,
        "You",
        "Review the cancellation path. 请说明结果和下一步。",
    );
    state.push_message(
        Role::Tool,
        "exec",
        "unknown · call-demo-1\nThe saved record has no verified exit status.",
    );
    state.push_message(
        Role::Assistant,
        "configured-model",
        "The recorded outcome is unknown. Check the workspace before retrying.\n\n```rust\ncancel.cancel();\nlet outcome = task.await?;\n```\n\nThe interface keeps the call identity and the recorded state visible.",
    );
    state.workspace = "/path/to/project".into();
    state.session_title = "Review cancellation".into();
    state.profile = "example".into();
    state.model = "configured-model".into();
    state.mode = "CODE".into();
    state.permissions = "Manual Approval".into();
    state.status = "Completed".into();
    state.usage = r#"{"input_tokens":1280,"output_tokens":256,"estimated":false}"#.into();
    state.context_window_tokens = Some(128_000);
    state.context_input_tokens = Some(1_280);
    state
        .editor
        .insert("Investigate the saved result.\nKeep existing user changes.");
    if let Some((title, markdown)) = markdown_fixture(scene) {
        state.clear_messages();
        state.editor.clear();
        state.session_title = title.into();
        state.permissions = "auto".into();
        state.workspace = "/workspace/saya-harness".into();
        state.push_message(Role::User, "You", format!("Preview · {title}"));
        state.push_message(Role::Assistant, "Markdown preview", markdown);
        state
            .editor
            .set_text("←/→ scenes · s/r source/render · Home/End · q exit");
        return state;
    }
    match scene {
        "approval" => {
            state.editor.clear();
            state.status = "Awaiting approval".into();
            let arguments = serde_json::json!({
                "argv": [
                    "/usr/bin/swift",
                    "/path/to/project/.tmp_vision.swift",
                    "/path/to/project/reference.png"
                ],
                "cwd": "/path/to/project",
                "timeout_seconds": 180,
                "__saya_command_analysis": {
                    "schema_version": 1,
                    "limits": {"max_arguments": 1024}
                }
            });
            let mut lines = vec![
                controller_text::approval_field(controller_text::TOOL_LABEL, "exec"),
                controller_text::approval_field(controller_text::CALL_LABEL, "call-demo-2"),
                controller_text::approval_field(controller_text::APPROVAL_LABEL, "approval-demo-2"),
                controller_text::approval_field(controller_text::MODE_LABEL, "Code"),
                controller_text::approval_field(
                    controller_text::WORKSPACE_LABEL,
                    "/path/to/project",
                ),
                String::new(),
                "Run a controlled image inspection command".into(),
                String::new(),
            ];
            lines.extend(
                tool_render::call("exec", &arguments, None)
                    .lines()
                    .map(str::to_owned),
            );
            lines.push(String::new());
            lines.push("y approve · n / Esc deny · /mode MODE changes mode".into());
            state.set_details(Details {
                title: crate::i18n::view::APPROVAL_REQUIRED.into(),
                lines,
            });
        }
        "attachments" => {
            state.clear_messages();
            state.editor.clear();
            state.session_title = "Inspect pasted assets".into();
            state.permissions = "auto".into();
            state.push_message(
                Role::User,
                "You",
                "Compare [Image 1], Audio: [Audio 1], Video: [Video 1], and File: [File 1].",
            );
            state.push_message(
                Role::Assistant,
                "configured-model",
                "I can inspect every enabled media reference. No media bytes were embedded in this preview.",
            );
            state.editor.set_text(
                "Also inspect [Image 2], Audio: [Audio 2], Video: [Video 2], and File: [File 2]",
            );
        }
        "question" => {
            state.editor.clear();
            state.status = "Awaiting input".into();
            state.set_details(Details {
                title: crate::i18n::view::QUESTION.into(),
                lines: vec![
                    "Which workspace should I verify before continuing?".into(),
                    String::new(),
                    "1. Current workspace".into(),
                    "2. /path/to/other-project".into(),
                    String::new(),
                    "Type a reply and press Enter. Ctrl+C cancels.".into(),
                ],
            });
        }
        "background" => {
            state.set_details(Details {
                title: controller_text::BACKGROUND_TITLE.into(),
                lines: vec![
                    controller_text::background_summary(2, 3),
                    String::new(),
                    "background-demo-build-1".into(),
                    "  exec · call-build".into(),
                    "  running".into(),
                    String::new(),
                    "background-demo-test-2".into(),
                    "  exec · call-test".into(),
                    "  running".into(),
                    String::new(),
                    "background-demo-docs-3".into(),
                    "  exec · call-docs".into(),
                    "  completed".into(),
                ],
            });
        }
        "tool" => {
            let arguments = serde_json::json!({
                "argv": ["/usr/bin/swift", ".tmp_vision.swift", "reference.png"],
                "cwd": "/path/to/project",
                "timeout_seconds": 180
            });
            let result = saya_core::contract::ToolResult {
                ok: true,
                data: Some(serde_json::json!({
                    "exit_code": 0,
                    "stdout": "image: 2732×2048\nvision: completed\n",
                    "stderr": "",
                    "stdout_truncated": false,
                    "stderr_truncated": false,
                    "timed_out": false,
                    "cancelled": false
                })),
                error: None,
                truncated: false,
            };
            state.set_details(Details {
                title: "Tool details".into(),
                lines: tool_render::call_and_result(
                    "exec",
                    &arguments,
                    &result,
                    Some(Duration::from_millis(1240)),
                )
                .lines()
                .map(str::to_owned)
                .collect(),
            });
        }
        "help" => {
            state.set_details(Details {
                title: controller_text::HELP_TITLE.into(),
                lines: COMMANDS
                    .iter()
                    .map(|command| format!("{} · {}", command.text, command.description))
                    .chain(
                        controller_text::HELP_LINES
                            .iter()
                            .map(|line| (*line).to_owned()),
                    )
                    .collect(),
            });
        }
        "commands" => {
            state.command_palette = Some(0);
        }
        "completion" => {
            state.editor.set_text("/permissions ");
            state
                .completion
                .update(state.editor.text(), state.editor.cursor(), true);
        }
        "memory" => {
            state.memory_panel = Some(MemoryPanel {
                settings:
                    "read true · inject true · write true · CODE · changes last for this process"
                        .into(),
                lines: vec![
                    "Memory overview".into(),
                    "  workspace · /path/to/project".into(),
                    "  facts · 12 current".into(),
                    String::new(),
                    "Recent changes".into(),
                    "  user preference · concise verification reports".into(),
                    "  project decision · native sandbox default".into(),
                ],
                ..Default::default()
            });
        }
        "skills" => state.picker_panel = Some(picker_fixture(PickerKind::Skills)),
        "mcps" => state.picker_panel = Some(picker_fixture(PickerKind::Mcps)),
        "plugins" => state.picker_panel = Some(picker_fixture(PickerKind::Plugins)),
        "sessions" => state.picker_panel = Some(picker_fixture(PickerKind::Sessions)),
        "idle" => {
            state.clear_messages();
            state.editor.clear();
            state.session_title = "New session".into();
            state.mode = "WORK".into();
            state.permissions = "auto".into();
            state.status = "Ready".into();
            state.usage.clear();
            state.context_input_tokens = None;
        }
        "working" | "completed" => {
            state.clear_messages();
            state.editor.clear();
            state.permissions = "auto".into();
            state.session_title = "Run activity · synthetic preview".into();
            state.push_message(
                Role::User,
                "You",
                "Refine the input frame and run feedback.",
            );
            if scene == "working" {
                state.status = "Thinking".into();
                state.activity_active = true;
                state.activity_frame = 3;
                state.push_message(
                    Role::Thinking,
                    "configured-model",
                    "Checking the layout, input cursor, and narrow terminal dimensions.",
                );
                if let Some(message) = state.messages.back_mut() {
                    message.streaming = true;
                }
            } else {
                state.status = "Completed".into();
                state.push_message(
                    Role::Assistant,
                    "configured-model",
                    "The input frame now has a fine mode accent and a dark right margin.\n\nThe statistics below are synthetic preview data.",
                );
                state.push_message(
                    Role::RunSummary,
                    "CODE",
                    "configured-model · 1m 19s · 143.5 tok/s · 12.3k tokens",
                );
            }
        }
        "compact" => {
            state
                .editor
                .set_text("Continue the review.\nKeep existing user changes.");
            state.context_input_tokens = Some(25_600);
            state.usage = r#"{"input_tokens":25600,"output_tokens":256,"estimated":false}"#.into();
        }
        "effort" => {
            state.editor.clear();
            state.thinking_effort = Some("max".into());
        }
        "sidebar" => {
            state.editor.clear();
            state.context_breakdown = [
                ("System", 8_000),
                ("Instructions", 4_000),
                ("Skills", 128),
                ("Preferences", 512),
                ("Memory", 1_024),
                ("Conversation", 8_192),
                ("Files", 1_024),
                ("Tools", 2_048),
                ("Protocol", 512),
            ]
            .into_iter()
            .map(|(label, tokens)| ContextEntry {
                label: label.into(),
                tokens,
            })
            .collect();
            state.skills_count = Some(3);
            state.todos = vec![
                TodoItem {
                    id: "context".into(),
                    content: "Verify Context accounting".into(),
                    status: TodoStatus::Completed,
                },
                TodoItem {
                    id: "tools".into(),
                    content: "Review tool rendering".into(),
                    status: TodoStatus::InProgress,
                },
            ];
        }
        "markdown" => {
            state.clear_messages();
            state.editor.clear();
            state.session_title = "Markdown tables and links".into();
            state.permissions = "auto".into();
            state.thinking_effort = Some("high".into());
            state.push_message(
                Role::User,
                "You",
                "请用表格总结当前显示能力，并附上参考链接。",
            );
            state.push_message(
                Role::Assistant,
                "configured-model",
                "## Markdown 显示\n\n支持 **加粗**、__strong__、*强调* 和 `inline code`。\n\n| 能力 | 状态 | 说明 |\n| :--- | :---: | ---: |\n| 表格 | **Ready** | 中英文自动换行 |\n| Unicode | `é` | 组合字符与中文路径 |\n| 链接 | [Guide](https://example.test/docs) | 目标地址可见 |\n\n参考 [Saya Harness](https://github.com/Ariakage/saya-harness)。\n\n> 窄窗口按可用宽度排列单元格。",
            );
            state.editor.set_text("继续检查窄窗口中的表格显示。");
        }
        "polish" => {
            state.clear_messages();
            state.editor.clear();
            state.session_title = "完善任务取消与恢复说明".into();
            state.permissions = "auto".into();
            state.thinking_effort = Some("high".into());
            state.push_message(
                Role::User,
                "You",
                "请检查取消后的会话恢复行为，并说明需要补充的验证。",
            );
            state.push_message(
                Role::Tool,
                "read_file · src/runtime.rs",
                "status: completed\nlines: 48–61\n工具返回的 `path` 和 **内容** 保持原文。",
            );
            state.push_message(
                Role::Assistant,
                "configured-model",
                "## 取消与恢复\n\n会话保留已提交的历史，恢复后等待新的用户输入。\n\n- **执行边界**  取消信号应传递到当前模型请求与工具任务。\n- **验证范围**  检查 `cancel.cancel()` 后是否完成后台清理。\n\n> 记录缺少退出状态时，先核对工作区中的实际结果。\n\n```rust\ncancel.cancel();\nlet result = runtime.wait().await?;\n```\n\n### 后续验证\n\n补充中文路径与组合字符 `é` 的覆盖，并验证 *不会重复执行* 已保存的工具调用。",
            );
            state.editor.set_text("补充取消过程中的恢复测试。");
        }
        "refined" => {
            state.clear_messages();
            state.editor.clear();
            state.session_title = "完善会话恢复与任务面板".into();
            state.workspace = "/workspace/saya-harness".into();
            state.permissions = "auto".into();
            state.status = "Completed".into();
            state.thinking_effort = Some("high".into());
            state.context_input_tokens = Some(25_600);
            state.context_breakdown = [
                ("System", 4_800),
                ("Instructions", 2_100),
                ("Skills", 860),
                ("Preferences", 160),
                ("Memory", 520),
                ("Conversation", 12_600),
                ("Files", 2_080),
                ("Tools", 1_920),
                ("Protocol", 560),
            ]
            .into_iter()
            .map(|(label, tokens)| ContextEntry {
                label: label.into(),
                tokens,
            })
            .collect();
            state.skills_count = Some(3);
            state.todos = vec![
                TodoItem {
                    id: "inspect".into(),
                    content: "核对会话与取消边界".into(),
                    status: TodoStatus::Completed,
                },
                TodoItem {
                    id: "present".into(),
                    content: "完善消息卡片与侧栏层次".into(),
                    status: TodoStatus::Completed,
                },
                TodoItem {
                    id: "verify".into(),
                    content: "运行终端交互验收".into(),
                    status: TodoStatus::InProgress,
                },
            ];
            state.push_message(
                Role::User,
                "You",
                "优化会话恢复的交互体验，保留任务进度与执行记录。",
            );
            state.push_message(Role::Thinking, "provider thought text", "Inspect the saved session and its current task snapshot. Verify cancellation and restoration before presenting the result.");
            let read_result = serde_json::json!({
                "ok": true,
                "data": {
                    "content": "// controlled preview line\n".repeat(25),
                    "start_line": 24,
                    "end_line": 48,
                    "version": "preview-version",
                    "truncated": false
                },
                "error": null,
                "truncated": false
            });
            state.push_message(
                Role::Tool,
                "read_file · read-session · completed",
                read_result.to_string(),
            );
            let tasks_result = serde_json::json!({
                "ok": true,
                "data": { "todos": state.todos },
                "error": null,
                "truncated": false
            });
            state.push_message(
                Role::Tool,
                "todo_write · task-progress · completed",
                tasks_result.to_string(),
            );
            state.push_message(Role::Assistant, "configured-model", "## 会话恢复与进度\n\n已整理恢复入口，任务清单和工具记录现在更容易核查。\n\n- **会话进度**  恢复后展示最近成功保存的任务状态。\n- **执行记录**  点击工具卡片可查看完整结果和调用标识。\n- **取消处理**  等待后台清理完成，再结束当前运行。\n\n```rust\nlet tasks = current_todos(&session.history);\nhandle.shutdown_background().await?;\n```\n\n接下来继续检查窄窗口、鼠标滚动和审批交互。");
            state.editor.set_text("继续验收窄窗口和恢复流程。");
        }
        "tasks" | "folded" | "expanded" => {
            state.clear_messages();
            state.editor.clear();
            state.session_title = "完善任务面板与取消验证".into();
            state.permissions = "auto".into();
            state.status = "Running".into();
            state.push_message(Role::User, "You", "实现待办进度面板，并检查取消边界。");
            let thinking = state.push_message(
                Role::Thinking,
                "provider thought text",
                "先检查已有运行状态与会话存储接口。\n随后通过受控用例验证取消后的清理结果。",
            );
            state.append_message(thinking, "");
            if scene == "expanded" {
                state.toggle_latest();
            }
            state.push_message(
                Role::Tool,
                "exec · call-demo-3 · completed",
                "running controlled tests\npermission boundary ok\nsession history ok\nstream cancellation ok\nbackground cleanup ok\nterminal restoration ok\nno duplicate execution\n8 checks completed",
            );
            if scene == "expanded" {
                state.toggle_latest();
            }
            state.push_message(
                Role::Assistant,
                "configured-model",
                "已完成取消链路检查，继续整理任务面板。",
            );
            state.todos = (0..12)
                .map(|index| TodoItem {
                    id: format!("task-{index}"),
                    content: match index {
                        0 => "检查会话存储与恢复接口".into(),
                        1 => "实现任务状态与侧栏进度显示".into(),
                        _ => format!("补充第 {index} 项取消和输入边界验证"),
                    },
                    status: match index {
                        0 => TodoStatus::Completed,
                        1 => TodoStatus::InProgress,
                        _ => TodoStatus::Pending,
                    },
                })
                .collect();
        }
        _ => {}
    }
    state
}

fn picker_fixture(kind: PickerKind) -> PickerPanel {
    let (title, items) = match kind {
        PickerKind::Skills => (
            picker_text::SKILLS_TITLE,
            vec![
                picker_item(
                    "review",
                    "review",
                    "Review changes with reproducible evidence",
                    true,
                ),
                picker_item(
                    "testing",
                    "testing",
                    "Run targeted local regression checks",
                    false,
                ),
            ],
        ),
        PickerKind::Mcps => (
            picker_text::MCPS_TITLE,
            vec![
                picker_item(
                    "filesystem",
                    "filesystem",
                    "stdio · 12 tools · enabled",
                    true,
                ),
                picker_item("github", "github", "HTTP · disabled", false),
            ],
        ),
        PickerKind::Plugins => (
            picker_text::PLUGINS_TITLE,
            vec![
                picker_item(
                    "saya-review",
                    "saya-review",
                    "v1.2.0 · enabled · workspace",
                    true,
                ),
                picker_item("saya-ci", "saya-ci", "v0.8.1 · disabled", false),
            ],
        ),
        PickerKind::Sessions => (
            picker_text::SESSIONS_TITLE,
            vec![
                picker_item(
                    "session-current",
                    "Verify approval modal",
                    "session-current · CODE · default · Ready",
                    true,
                ),
                picker_item(
                    "session-previous",
                    "Implement background processes",
                    "session-previous · WORK · default · Completed",
                    false,
                ),
            ],
        ),
    };
    PickerPanel {
        kind,
        title: title.into(),
        items,
        selected: 0,
        scroll: 0,
        page_rows: 0,
        hint: picker_text::DRAFT_RETAINED.into(),
    }
}

fn picker_item(id: &str, label: &str, description: &str, active: bool) -> PickerItem {
    PickerItem {
        id: id.into(),
        label: label.into(),
        description: description.into(),
        details: vec![description.into()],
        active,
    }
}

fn markdown_fixture(scene: &str) -> Option<(&'static str, &'static str)> {
    Some(match scene {
        "syntax" => (
            "Markdown · headings and inline styles",
            concat!(
                r#"# ATX level 1
## ATX level 2
### ATX level 3
#### ATX level 4
##### ATX level 5
###### ATX level 6

Setext level 1
==============

Setext level 2
--------------

普通段落会根据可用宽度自动换行。Unicode 示例包括中文、组合字符 é、emoji 👩‍💻，缩小窗口后可以检查边界。

**Bold 粗体** · *Italic 斜体* · ***Bold italic 粗斜体*** · ~~Deleted 删除线~~

__Underscore bold__ · `inline_code("**literal**")` · Escaped \*literal\*

---

Paragraph after the horizontal rule. Two trailing spaces create a hard break."#,
                "  \nThis starts on the next displayed line.\n"
            ),
        ),
        "syntax-blocks" => (
            "Markdown · highlighted code and HTML source",
            r#"## Rust
```rust
fn greet(name: &str) -> String {
    // Preserve source text and color language tokens.
    format!("Hello, {name}!")
}
```

## Python
```python
def mean(values):
    """Compute the sample mean."""
    return sum(values) / len(values)
```

## JavaScript
```javascript
const message = "你好，Saya";
console.log(message);
```

## JSON
```json
{"mode": "CODE", "preview": true, "items": [1, 2, 3]}
```

## Indented code

    const literal = "**source stays visible**";
    render(literal);

## HTML source
<section class="example">
  <strong>HTML is displayed as source.</strong>
</section>
"#,
        ),
        "syntax-lists" => (
            "Markdown · nested lists and quotes",
            r#"## Unordered lists
- First item
  - Nested item
    - Third level with **bold text**
  - Another child
- Second item

## Ordered lists
1. Inspect the workspace
   1. Read the project instructions
   2. Review the implementation
2. Implement the change
3. Validate the result

## Task lists
- [x] Parse CommonMark and GFM
- [x] Render Unicode safely
- [ ] Review the local preview
  - [x] Test narrow windows
  - [ ] Test your preferred terminal

## Nested quotes
> First level
>
> > Second level with *emphasis*
> >
> > > Third level
>
> Back to the first level.
"#,
        ),
        "syntax-links" => (
            "Markdown · tables, links and image descriptions",
            r#"## GFM table
| Feature | State | Details |
| :--- | :---: | ---: |
| **Strong** | Ready | Unicode 中文 |
| Links | [Guide](https://example.com/docs) | Target visible |
| Code | `left\|right` | Escaped pipe |
| Math | $x^2 + \alpha$ | Inline formula |

## Links and autolinks
[Saya Harness](https://github.com/Ariakage/saya-harness "Repository")

<https://example.com/reference> · <reader@example.com>

Bare URL https://example.com/path_(one)?q=value

[Reference-style link][guide]

[guide]: https://example.com/guide "Guide"

## Image descriptions
![Diagram alternative text](https://example.com/diagram.png "Description only")

The alternative text and image URL are displayed as a link description.
"#,
        ),
        "footnotes" => (
            "Markdown · numbered footnotes and navigation",
            r#"# Footnotes

Click a numbered reference [^runtime] to jump to its definition. Click its definition number or press **b** to return in the debug preview.

Numbering follows first reference order, including a named note [^unicode]. The same note can be referenced again [^runtime].

## Intermediate content
1. Parse the message.
2. Keep semantic anchors attached to the rendered text.
3. Recalculate positions after a window resize.

> A literal code marker `[^runtime]` has no navigation action.

| Context | Reference |
| --- | --- |
| Table cell | Reference inside a table [^unicode] |

Undefined references remain visible [^undefined].

[^runtime]: Runtime note. **Formatting** and links such as [reference](https://example.com/runtime) are retained.

    A second paragraph can belong to the same definition.

[^unicode]: Unicode note. 中文、é and 👩‍💻 remain readable in narrow windows.
"#,
        ),
        "math" => (
            "Markdown · Unicode math and source fallback",
            r#"# Unicode mathematics

Inline dollar notation $x^2 + y_1 = \alpha$ and backslash notation \(E = mc^2\).

## Display environments
$$\frac{a+b}{c} + \sqrt{x^2 + y^2}$$

\[\sum_{i=1}^{n} x_i\]

## LaTeX fence
```latex
\begin{pmatrix}
a & b \\
c & d
\end{pmatrix}
```

## Math fence
```math
\boxed{\alpha + \beta = \gamma}
```

## Source fallback
Unknown macros and unavailable Unicode scripts retain their source.

```latex
\unknowncommand{x}
```

```math
x_q + \frac{missing}
```
"#,
        ),
        "mermaid" => (
            "Mermaid · flowchart",
            r#"# Flowchart
```mermaid
flowchart TD
    A[读取任务] --> B{验证通过?}
    B -->|Yes| C[完成构建]
    B -->|No| D[检查原因]
    D --> B
```

The diagram uses terminal cells. Narrow layouts fall back to the source when the complete chart cannot fit.
"#,
        ),
        "mermaid-sequence" => (
            "Mermaid · sequence diagram",
            r#"# Sequence diagram
```mermaid
sequenceDiagram
    participant User
    participant Saya
    participant Model
    User->>Saya: Submit task
    Saya->>Model: Request response
    Model-->>Saya: Return answer
    Saya-->>User: Display result
```
"#,
        ),
        "mermaid-state" => (
            "Mermaid · state diagram",
            r#"# State diagram
```mermaid
stateDiagram-v2
    [*] --> Idle
    Idle --> Running: Submit
    Running --> Completed: Success
    Running --> Failed: Error
    Completed --> [*]
    Failed --> Idle: Retry
```
"#,
        ),
        "mermaid-class" => (
            "Mermaid · class diagram",
            r#"# Class diagram
```mermaid
classDiagram
    class Runtime {
        +run()
        +cancel()
    }
    class Tool {
        +execute()
    }
    Runtime --> Tool : dispatch
```
"#,
        ),
        "mermaid-er" => (
            "Mermaid · entity relationship",
            r#"# Entity relationship
```mermaid
erDiagram
    SESSION ||--o{ RUN : contains
    RUN ||--o{ TOOL_CALL : records
```
"#,
        ),
        "mermaid-fallback" => (
            "Mermaid · error and unsupported fallback",
            r#"# Diagram source fallback

## Incomplete syntax
```mermaid
flowchart TD
A[unfinished --> B
```

## Unsupported family
```mermaid
pie title Distribution
    "Build" : 60
    "Review" : 40
```

## Malformed diagram
```mermaid
this is not a valid Mermaid diagram
```
"#,
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_and_completed_scenes_preserve_live_status_and_fixed_snapshot_phase() {
        for scene in ["working", "completed"] {
            let index = SCENES
                .iter()
                .position(|candidate| *candidate == scene)
                .unwrap();
            let state = scene_state(index, MarkdownDisplay::Render);
            assert!(state.session_title.contains("synthetic preview"));
            if scene == "working" {
                assert_eq!(state.status, "Thinking");
                assert!(state.activity_visible());
                assert_eq!(state.activity_frame, 3);
                assert!(
                    state
                        .messages
                        .iter()
                        .any(|message| { message.role == Role::Thinking && message.streaming })
                );
            } else {
                assert_eq!(state.status, "Completed");
                assert!(!state.activity_visible());
                let summary = state.messages.back().unwrap();
                assert_eq!(summary.role, Role::RunSummary);
                assert_eq!(summary.label, "CODE");
                assert!(summary.text.contains("1m 19s · 143.5 tok/s · 12.3k tokens"));
            }
        }
    }

    #[test]
    fn every_modal_and_sidebar_has_a_preview_scene() {
        for scene in [
            "approval",
            "attachments",
            "question",
            "background",
            "help",
            "commands",
            "completion",
            "memory",
            "skills",
            "mcps",
            "plugins",
            "sessions",
            "tool",
            "sidebar",
        ] {
            assert!(SCENES.contains(&scene), "missing preview scene {scene}");
            let state = fixture(scene);
            match scene {
                "approval" | "question" | "background" | "help" | "tool" => {
                    assert!(state.details.is_some(), "{scene}");
                }
                "commands" => assert!(state.command_palette.is_some()),
                "attachments" => {
                    assert!(state.editor.text().contains("[Image 2]"));
                    assert!(
                        state
                            .messages
                            .iter()
                            .any(|message| message.text.contains("[File 1]"))
                    );
                }
                "completion" => assert!(state.completion.visible()),
                "memory" => assert!(state.memory_panel.is_some()),
                "skills" | "mcps" | "plugins" | "sessions" => {
                    assert!(state.picker_panel.is_some(), "{scene}");
                }
                "sidebar" => {
                    assert!(!state.todos.is_empty());
                    assert!(!state.context_breakdown.is_empty());
                }
                _ => unreachable!(),
            }
        }
    }
}
