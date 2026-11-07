//! Render production widgets from a synthetic state using TestBackend.
use ratatui::{Terminal, backend::TestBackend};
use saya_tui::commands::COMMANDS;
use saya_tui::view::{self, Details, Role, State};
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
    state.profile = "example".into();
    state.model = "configured-model".into();
    state.mode = "CODE".into();
    state.permissions = "Manual Approval".into();
    state.status = "Completed".into();
    state.usage = r#"{"input_tokens":1280,"output_tokens":256,"estimated":false}"#.into();
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
            "render_preview [WIDTH HEIGHT] [chat|approval|tool|commands|completion]"
        )?;
        return Ok(());
    }
    if args.len() > 3 {
        return Err(
            "usage: render_preview [WIDTH HEIGHT] [chat|approval|tool|commands|completion]".into(),
        );
    }
    let width = dimension(args.first(), 120, 240)?;
    let height = dimension(args.get(1), 30, 80)?;
    let scene = args.get(2).map(String::as_str).unwrap_or("chat");
    if !matches!(
        scene,
        "chat" | "approval" | "tool" | "commands" | "completion"
    ) {
        return Err("scene must be chat, approval, tool, commands or completion".into());
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
