//! Render the unified session view from a synthetic state using TestBackend.
use ratatui::{Terminal, backend::TestBackend};
use saya_core::tools::todo::{TodoItem, TodoStatus};
use saya_tui::commands::COMMANDS;
use saya_tui::view::{self, ContextEntry, Details, Role, State};
use std::{error::Error, io::Write};
use unicode_width::UnicodeWidthStr;

fn fixture(scene: &str) -> State {
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
    match scene {
        "approval" => {
            state.editor.clear();
            state.status = "Awaiting approval".into();
            state.set_details(Details {
                title: "Approval required".into(),
                lines: vec![
                    "Tool read_file".into(),
                    "Call call-demo-2".into(),
                    "Approval approval-demo-2".into(),
                    "Mode Code".into(),
                    "Workspace /path/to/project".into(),
                    "Read src/runtime.rs".into(),
                    "{\n  \"path\": \"src/runtime.rs\",\n  \"start_line\": 1,\n  \"line_count\": 30\n}".into(),
                    "y approve · n / Esc deny · /mode MODE changes mode".into(),
                ],
            });
        }
        "tool" => {
            state.set_details(Details {
                title: "Tools · UI snapshot".into(),
                lines: vec![
                    "Session session-demo-1 · committed change 12 · event 8".into(),
                    "Read by the UI from stored records. Tool calls are identified by run_id and call_id.".into(),
                    "[".into(),
                    "  {".into(),
                    "    \"run_id\": \"run-demo-1\",".into(),
                    "    \"call_id\": \"call-demo-1\",".into(),
                    "    \"tool\": \"exec\",".into(),
                    "    \"state\": \"unknown\"".into(),
                    "  }".into(),
                    "]".into(),
                ],
            });
        }
        "commands" => {
            state.set_details(Details {
                title: "Commands".into(),
                lines: COMMANDS
                    .iter()
                    .enumerate()
                    .map(|(index, suggestion)| {
                        format!(
                            "{} {} · {}",
                            if index == 0 { "›" } else { " " },
                            suggestion.text,
                            suggestion.description
                        )
                    })
                    .chain(std::iter::once("↑/↓ select · Enter run · Esc close".into()))
                    .collect(),
            });
        }
        "completion" => {
            state.editor.set_text("/permissions ");
            state
                .completion
                .update(state.editor.text(), state.editor.cursor(), true);
        }
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
        }
        "polish" | "markdown" => {
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

fn dimension(value: Option<&String>, default: u16, maximum: u16) -> Result<u16, Box<dyn Error>> {
    let value = value
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(default);
    if value == 0 || value > maximum {
        return Err(format!("preview dimension must be 1..={maximum}").into());
    }
    Ok(value)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = std::env::args_os()
        .skip(1)
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| "preview arguments must be UTF-8")
        })
        .collect::<Result<Vec<_>, _>>()?;
    if args.iter().any(|argument| argument == "--help") {
        writeln!(
            std::io::stdout().lock(),
            "render_preview [WIDTH HEIGHT] [chat|approval|tool|commands|completion|idle|compact|effort|sidebar|polish|markdown|tasks|folded|expanded|refined]"
        )?;
        return Ok(());
    }
    if args.len() > 3 {
        return Err(
            "usage: render_preview [WIDTH HEIGHT] [chat|approval|tool|commands|completion|idle|compact|effort|sidebar|polish|markdown|tasks|folded|expanded|refined]".into(),
        );
    }
    let width = dimension(args.first(), 120, 240)?;
    let height = dimension(args.get(1), 30, 80)?;
    let scene = args.get(2).map(String::as_str).unwrap_or("chat");
    if !matches!(
        scene,
        "chat"
            | "approval"
            | "tool"
            | "commands"
            | "completion"
            | "idle"
            | "compact"
            | "effort"
            | "sidebar"
            | "polish"
            | "markdown"
            | "tasks"
            | "folded"
            | "expanded"
            | "refined"
    ) {
        return Err(
            "scene must be chat, approval, tool, commands, completion, idle, compact, effort, sidebar, polish, markdown, tasks, folded, expanded or refined"
                .into(),
        );
    }
    let mut state = fixture(scene);
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    terminal.draw(|frame| view::draw(frame, &mut state))?;
    let buffer = terminal.backend().buffer();
    let mut output = String::new();
    for row in 0..height {
        let mut column = 0;
        let mut line = String::new();
        while column < width {
            let symbol = buffer[(column, row)].symbol();
            line.push_str(symbol);
            let cells = UnicodeWidthStr::width(symbol)
                .max(1)
                .min(usize::from(width - column));
            column += cells as u16;
        }
        output.push_str(line.trim_end());
        output.push('\n');
    }
    std::io::stdout().lock().write_all(output.as_bytes())?;
    Ok(())
}
