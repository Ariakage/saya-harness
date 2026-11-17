use super::overlays::*;
use super::*;

/// Render a state snapshot. Terminal controls are displayed as escaped text.
/// This function performs no model, storage or terminal I/O.
pub fn draw(frame: &mut Frame<'_>, state: &mut State) {
    state.enforce_limits();
    state.enforce_details_limits();
    state.message_hits.clear();
    state.footnote_hits.clear();
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
        state.selection.reset();
        frame.render_widget(Paragraph::new(ui_text::TERMINAL_TOO_SMALL), area);
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
    draw_modal_details(frame, state);
    if state.command_palette.is_none()
        && state.completion.visible()
        && state.details.as_ref().is_some_and(details_modal)
        && let Some(body) = state.transcript_viewport
    {
        draw_completion(frame, state, body);
    }
    draw_command_palette(frame, state);
    memory::draw(frame, state);
    picker::draw(frame, state);
    let areas = if state.memory_panel.is_some()
        || state.picker_panel.is_some()
        || state.command_palette.is_some()
        || state.completion_viewport.is_some()
    {
        [None, None]
    } else {
        [state.transcript_viewport, state.details_viewport]
    };
    state.selection.render(frame.buffer_mut(), areas);
}

pub(super) fn details_modal(details: &Details) -> bool {
    matches!(
        details.title.as_str(),
        ui_text::APPROVAL_REQUIRED | ui_text::QUESTION | controller_text::BACKGROUND_TITLE
    )
}

pub(super) fn draw_modal_details(frame: &mut Frame<'_>, state: &mut State) {
    let Some(details) = &state.details else {
        return;
    };
    if !details_modal(details) {
        return;
    }
    let outer = state
        .transcript_viewport
        .filter(|area| area.height >= 3)
        .unwrap_or_else(|| frame.area());
    if outer.width < 8 || outer.height < 3 {
        state.details_page_rows = 1;
        return;
    }
    let width = outer
        .width
        .saturating_sub(4)
        .clamp(32, 104)
        .min(outer.width.max(1));
    let natural = details
        .lines
        .iter()
        .map(|line| line.lines().count().max(1))
        .sum::<usize>()
        .saturating_add(3);
    let maximum_height = outer.height.saturating_sub(2).max(1);
    let minimum_height = maximum_height.min(6);
    let height = u16::try_from(natural)
        .unwrap_or(u16::MAX)
        .clamp(minimum_height, maximum_height);
    let area = Rect::new(
        outer.x + (outer.width.saturating_sub(width)) / 2,
        outer.y + (outer.height.saturating_sub(height)) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    state.details_viewport = Some(area);
    draw_details(
        frame,
        details,
        &mut state.details_scroll,
        &mut state.details_page_rows,
        area,
        true,
    );
}

pub(super) fn draw_body(frame: &mut Frame<'_>, state: &mut State, body: Rect) {
    if state.details.as_ref().is_some_and(details_modal) {
        draw_transcript(frame, state, body);
    } else if state.details.is_some() && body.width >= 98 {
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
                false,
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
            false,
        );
    } else {
        state.details_page_rows = 10;
        draw_transcript(frame, state, body);
    }
}

pub(super) fn draw_session(frame: &mut Frame<'_>, state: &mut State, area: Rect) {
    let brand = Style::default()
        .fg(STRONG_FOREGROUND)
        .add_modifier(Modifier::BOLD);
    let title = if area.width < 44 {
        Line::styled(common::APP_NAME, brand)
    } else {
        Line::from(vec![
            Span::styled(common::APP_NAME, brand),
            Span::raw(" "),
            Span::styled(
                format!("(v{})", env!("CARGO_PKG_VERSION")),
                Style::default().fg(MUTED),
            ),
        ])
    };
    let hint = if state.copied_notice {
        Line::styled(
            format!(" {} ", common::COPIED),
            Style::default()
                .fg(BACKGROUND)
                .bg(GREEN)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        let hint = if state.completion.visible() {
            if area.width < 28 {
                ui_text::COMPLETION_SHORT
            } else {
                ui_text::COMPLETION_KEYS
            }
        } else if state.editor.text().is_empty() && state.command_palette.is_none() {
            if area.width >= 70 {
                ui_text::TAB_MODE
            } else if area.width >= 44 {
                ui_text::TAB_MODE_SHORT
            } else {
                ui_text::CTRL_P
            }
        } else if area.width < 44 {
            ui_text::CTRL_P
        } else {
            ui_text::COMMANDS_HINT
        };
        Line::styled(hint, Style::default().fg(MUTED))
    };
    draw_split_line(
        frame,
        title,
        hint,
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
        inner.height.saturating_sub(
            if state
                .details
                .as_ref()
                .is_some_and(|details| !details_modal(details))
            {
                2
            } else {
                1
            },
        ),
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

pub(super) fn draw_sidebar(frame: &mut Frame<'_>, state: &State, area: Rect) {
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
    let server_rows = if area.height >= 32 {
        state.mcp_servers.len().min(MAX_MCP_STATUS_SERVERS).min(5)
    } else {
        0
    };
    let tasks_row = if compact_tasks { 12 } else { 20 + server_rows };
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
        Span::styled(ui_text::CONTEXT, heading),
        Span::styled(
            if state.context_breakdown.is_empty() {
                ui_text::CONTEXT_UNKNOWN
            } else if state.context_breakdown_estimated {
                ui_text::CONTEXT_ESTIMATED
            } else {
                ""
            },
            Style::default().fg(MUTED),
        ),
    ]);
    lines[3] = Line::styled(
        format!(
            "{} {}",
            ui_text::CONTEXT_WINDOW,
            state
                .context_window_tokens
                .filter(|tokens| *tokens > 0)
                .map(common::token_label)
                .unwrap_or_else(|| "—".into())
        ),
        Style::default().fg(MUTED),
    );
    if state.context_breakdown.is_empty() {
        let text = if matches!(state.status.as_str(), common::THINKING | common::RESPONDING) {
            ui_text::CONTEXT_AWAITING_USAGE
        } else {
            ui_text::CONTEXT_UNKNOWN_BEFORE_REQUEST
        };
        lines[4] = Line::styled(text, Style::default().fg(MUTED));
    } else {
        for (index, line) in context_rows(state, usize::from(content.width))
            .into_iter()
            .take(context_limit)
            .enumerate()
        {
            lines[index + 4] = line;
        }
        if state.context_breakdown.len() > context_limit {
            lines[total_row] = Line::styled(ui_text::CONTEXT_MORE, Style::default().fg(MUTED));
        } else {
            let total = state
                .context_breakdown
                .iter()
                .map(|entry| u128::from(entry.tokens))
                .sum::<u128>();
            let summary = match u64::try_from(total) {
                Ok(total) => format!(
                    "{} {}{} {}",
                    ui_text::CONTEXT_TOTAL,
                    if state.context_breakdown_estimated {
                        "~"
                    } else {
                        ""
                    },
                    common::token_label(total),
                    sidebar_percentage(total, state.context_window_tokens)
                ),
                Err(_) => format!(
                    "{} {}",
                    ui_text::CONTEXT_TOTAL,
                    ui_text::CONTEXT_ESTIMATE_OVERFLOW
                ),
            };
            lines[total_row] = Line::styled(summary, Style::default().fg(MUTED));
        }
    }
    lines[extensions_row] = Line::styled(ui_text::EXTENSIONS, heading);
    lines[extensions_row + 1] = mcp_summary(state, server_rows);
    for (index, server) in state.mcp_servers.iter().take(server_rows).enumerate() {
        lines[extensions_row + 2 + index] = mcp_server_line(server, usize::from(content.width));
    }
    lines[extensions_row + 2 + server_rows] = Line::styled(
        match state.plugins_enabled {
            Some(true) => ui_text::PLUGINS_ENABLED,
            Some(false) => ui_text::PLUGINS_DISABLED,
            None => ui_text::PLUGINS_UNKNOWN,
        },
        Style::default().fg(MUTED),
    );
    lines[extensions_row + 3 + server_rows] = Line::styled(
        match state.skills_count {
            Some(count) => format!("{} {count} {}", ui_text::SKILLS, ui_text::SKILLS_DISCOVERED),
            None => format!("{} —", ui_text::SKILLS),
        },
        Style::default().fg(MUTED),
    );
    let completed = state
        .todos
        .iter()
        .filter(|todo| todo.status == TodoStatus::Completed)
        .count();
    lines[tasks_row] = Line::from(vec![
        Span::styled(ui_text::TASKS, heading),
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
        lines[task_end] = Line::styled(ui_text::NO_TASKS, Style::default().fg(MUTED));
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
                ui_text::more_tasks(state.todos.len() - visible),
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
        lines[details_row] = Line::styled(ui_text::DETAILS, heading);
    }
    for (index, (label, value)) in [
        (ui_text::PROFILE, state.profile.as_str()),
        (ui_text::MODEL, state.model.as_str()),
        (ui_text::MODE, state.mode.as_str()),
        (ui_text::PERMISSIONS, state.permissions.as_str()),
        (
            ui_text::EFFORT,
            state.thinking_effort.as_deref().unwrap_or("—"),
        ),
        (ui_text::STATUS, state.status.as_str()),
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

pub(super) fn mcp_state(
    state: McpServerState,
) -> (&'static str, &'static str, ratatui::style::Color) {
    match state {
        McpServerState::Configured => ("○", ui_text::MCP_CONFIGURED, MUTED),
        McpServerState::Connecting => ("◐", ui_text::MCP_CONNECTING, AMBER),
        McpServerState::Connected => ("●", ui_text::MCP_CONNECTED, GREEN),
        McpServerState::Stopped => ("○", ui_text::MCP_STOPPED, MUTED),
        McpServerState::NeedsAuth => ("!", ui_text::MCP_NEEDS_AUTH, AMBER),
        McpServerState::Failed => ("×", ui_text::MCP_FAILED, RED),
    }
}
pub(super) fn mcp_server_line(server: &McpServerStatus, width: usize) -> Line<'static> {
    let (icon, status, color) = mcp_state(server.state);
    let name = safe_line(&server.server[..prefix_boundary(&server.server, 128)]);
    let transport = match server.transport {
        McpTransport::Stdio => common::TRANSPORT_STDIO,
        McpTransport::Http => common::TRANSPORT_HTTP,
    };
    let tools = ui_text::tool_count(server.tools);
    let detail = if width >= 36 {
        format!("{status} · {transport} · {tools}")
    } else {
        format!("{status} · {tools}")
    };
    let name_width = width.saturating_sub(detail.width() + 3);
    let name = crop_display(&name, 0, name_width);
    Line::from(vec![
        Span::styled(format!("{icon} "), Style::default().fg(color)),
        Span::styled(name, Style::default().fg(FOREGROUND)),
        Span::styled(format!(" {detail}"), Style::default().fg(color)),
    ])
}

pub(super) fn mcp_summary(state: &State, visible: usize) -> Line<'static> {
    let count = state.mcp_servers.len().min(MAX_MCP_STATUS_SERVERS);
    if count == 0 {
        return Line::styled(ui_text::MCP_NO_SERVERS, Style::default().fg(MUTED));
    }
    if visible == 0 {
        let first = &state.mcp_servers[0];
        let (_, status, color) = mcp_state(first.state);
        return Line::styled(
            ui_text::mcp_summary(
                status,
                count,
                &safe_line(&first.server[..prefix_boundary(&first.server, 128)]),
            ),
            Style::default().fg(color),
        );
    }
    Line::styled(
        ui_text::mcp_count(count, visible),
        Style::default().fg(MUTED),
    )
}

pub(super) fn context_rows(state: &State, width: usize) -> Vec<Line<'static>> {
    let entries = state
        .context_breakdown
        .iter()
        .take(MAX_CONTEXT_ENTRIES)
        .map(|entry| {
            let metric = format!(
                "{} {}",
                common::token_label(entry.tokens),
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

pub(super) fn sidebar_percentage(input: u64, window: Option<u64>) -> String {
    let Some(window) = window.filter(|window| *window > 0) else {
        return "—".into();
    };
    if input > window {
        return ui_text::OVER_100_PERCENT.into();
    }
    let tenths = u128::from(input) * 1000 / u128::from(window);
    if input > 0 && tenths == 0 {
        ui_text::LESS_THAN_POINT_ONE_PERCENT.into()
    } else if tenths % 10 == 0 {
        format!("{}%", tenths / 10)
    } else {
        format!("{}.{}%", tenths / 10, tenths % 10)
    }
}

pub(super) fn context_bar(input: u64, window: Option<u64>, width: usize) -> (String, String) {
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

pub(super) fn context_percentage(input: u64, window: Option<u64>) -> Option<u64> {
    window.filter(|window| *window > 0).map(|window| {
        if input > window {
            101
        } else {
            (u128::from(input) * 100 / u128::from(window)) as u64
        }
    })
}

pub(super) fn percentage_label(percent: Option<u64>) -> String {
    match percent {
        Some(101) => ui_text::OVER_100_PERCENT.into(),
        Some(value) => format!("{value}%"),
        None => "—".into(),
    }
}

pub(super) fn draw_split_line(
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

pub(super) fn draw_status(frame: &mut Frame<'_>, state: &State, area: Rect) {
    let status_value = status_text::display(&state.status);
    let status = if area.width < 40 {
        match status_value.as_str() {
            common::AWAITING_APPROVAL => ui_text::APPROVAL_SHORT,
            common::AWAITING_INPUT => ui_text::STATUS_INPUT,
            common::CANCELLING => ui_text::STATUS_CANCEL,
            common::COMPLETED => ui_text::STATUS_DONE,
            _ => status_value.as_str(),
        }
    } else {
        status_value.as_str()
    };
    let permissions = if area.width < 48 {
        permission_text::short(&state.permissions)
    } else {
        state.permissions.as_str()
    };
    let ready = matches!(state.status.as_str(), common::READY | common::COMPLETED);
    let status_color = match status_value.as_str() {
        common::READY | common::COMPLETED => GREEN,
        common::FAILED => RED,
        common::AWAITING_APPROVAL
        | common::AWAITING_INPUT
        | "NeedsConfirmation"
        | "NeedsInput"
        | "LimitReached" => AMBER,
        common::RUNNING
        | common::REVIEWING
        | common::THINKING
        | common::RESPONDING
        | common::COMPACTING => BLUE,
        _ => MUTED,
    };
    let mut label = Line::from(vec![
        Span::styled(
            if ready { "✓ " } else { "● " },
            Style::default().fg(status_color),
        ),
        Span::styled(safe_line(status), Style::default().fg(MUTED)),
    ]);
    let permissions = Line::styled(
        safe_line(permissions),
        Style::default().fg(if state.permissions == permission_text::AUTO {
            MUTED
        } else {
            SOFT_AMBER
        }),
    );
    let permission_width = permissions
        .width()
        .min(usize::from(area.width).saturating_sub(7));
    let available = usize::from(area.width)
        .saturating_sub(permission_width + usize::from(permission_width > 0));
    if state.activity_visible() && label.width() + 9 <= available {
        let accent = mode_color(&state.mode);
        let mut particles = vec![Span::styled(" ", Style::default().fg(accent)); 8];
        for age in (0..=3).rev() {
            let Some(previous_frame) = state.activity_frame.checked_sub(age) else {
                continue;
            };
            let position = ((previous_frame % 14) as usize).abs_diff(7);
            let modifier = if age == 0 {
                Modifier::empty()
            } else {
                Modifier::DIM
            };
            particles[position] = Span::styled(
                ["█", "▓", "▒", "░"][age as usize],
                Style::default().fg(accent).add_modifier(modifier),
            );
        }
        label.spans.push(Span::raw(" "));
        label.spans.extend(particles);
    }
    draw_split_line(frame, label, permissions, area, 6);
}

pub(super) fn draw_footer(frame: &mut Frame<'_>, state: &State, area: Rect) {
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

pub(super) fn context_meter(state: &State, short: bool) -> String {
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
        return format!("{ring} {} {estimated}{percentage}", ui_text::METER_INPUT);
    }
    let input = state
        .context_input_tokens
        .map(common::token_label)
        .unwrap_or_else(|| "—".into());
    let window = window
        .map(common::token_label)
        .unwrap_or_else(|| "—".into());
    format!(
        "{ring} {} {estimated}{input}/{window} ({percentage})",
        ui_text::METER_INPUT
    )
}

pub(super) fn safe_line(text: &str) -> String {
    safe_text(text)
        .replace('\n', "\\n")
        .graphemes(true)
        .map(display_grapheme)
        .collect()
}

pub(super) fn role_style(role: Role) -> Style {
    Style::default().fg(match role {
        Role::User => BLUE,
        Role::Assistant => FOREGROUND,
        Role::Tool => AMBER,
        Role::Thinking => MUTED,
        Role::System | Role::RunSummary => MUTED,
    })
}

pub(super) fn attachment_line(text: &str, base: Style) -> Line<'static> {
    let ranges = attachments::token_ranges(text);
    if ranges.is_empty() {
        return Line::styled(text.to_owned(), base);
    }
    let mut spans = Vec::with_capacity(ranges.len() * 2 + 1);
    let mut copied = 0;
    for range in ranges {
        if copied < range.start {
            spans.push(Span::styled(text[copied..range.start].to_owned(), base));
        }
        let style = match range.kind {
            attachments::Kind::Image => base.fg(BLUE).add_modifier(Modifier::BOLD),
            attachments::Kind::Audio => base.fg(AMBER).add_modifier(Modifier::BOLD),
            attachments::Kind::Video => base.fg(HEADING).add_modifier(Modifier::BOLD),
            attachments::Kind::File => base.fg(GREEN).add_modifier(Modifier::BOLD),
        };
        spans.push(Span::styled(text[range.start..range.end].to_owned(), style));
        copied = range.end;
    }
    if copied < text.len() {
        spans.push(Span::styled(text[copied..].to_owned(), base));
    }
    Line::from(spans)
}

pub(super) fn draw_transcript(frame: &mut Frame<'_>, state: &mut State, mut inner: Rect) {
    state.message_hits.clear();
    state.footnote_hits.clear();
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
    let mut footnotes = VecDeque::new();
    let render_candidates = state.render_candidates();
    state.render_cache.retain(&render_candidates);
    if state.dropped_messages > 0 {
        append_wrapped(
            &ui_text::earlier_viewport_messages(state.dropped_messages),
            width,
            Style::default().fg(MUTED),
            &mut lines,
            &mut line_count,
        );
    }
    for message in &state.messages {
        if message.role == Role::RunSummary {
            let mut summary = Line::from(vec![
                Span::styled(
                    safe_line(&message.label),
                    Style::default().fg(mode_color(&message.label)),
                ),
                Span::styled(
                    format!(" · {}", safe_line(&message.text)),
                    Style::default().fg(MUTED),
                ),
            ]);
            if message.truncated {
                summary.spans.push(Span::styled(
                    common::TRUNCATED_SUFFIX,
                    Style::default().fg(MUTED),
                ));
            }
            append_wrapped_line(
                summary,
                width,
                &mut lines,
                &mut line_count,
                MAX_RENDER_LINES,
            );
            append_wrapped("", width, Style::default(), &mut lines, &mut line_count);
            continue;
        }
        let role_label = match message.role {
            Role::User => ui_text::YOU,
            Role::Assistant => ui_text::SAYA,
            Role::Tool => ui_text::TOOL,
            Role::Thinking => ui_text::THINKING,
            Role::System => ui_text::SYSTEM,
            Role::RunSummary => "",
        };
        let can_fold = foldable(message);
        let expanded = state.expanded_messages.contains(&message.id);
        let summary = message.tool_summary.clone().or_else(|| {
            (message.role == Role::Tool)
                .then(|| summarize(&message.text))
                .flatten()
        });
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
                ui_text::RUNNING
            } else if message.label == common::UNAVAILABLE {
                common::UNAVAILABLE
            } else {
                ui_text::DONE
            };
            heading.spans.push(Span::styled(
                format!(" · {status}"),
                Style::default().fg(if message.streaming { BLUE } else { MUTED }),
            ));
            if message.truncated {
                heading.spans.push(Span::styled(
                    common::TRUNCATED_SUFFIX,
                    Style::default().fg(AMBER),
                ));
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
                heading.spans.push(Span::styled(
                    common::TRUNCATED_SUFFIX,
                    Style::default().fg(AMBER),
                ));
            }
            let (marker, color) = if summary.running {
                (" · ● ", BLUE)
            } else if summary.ok {
                (" · ✓ ", GREEN)
            } else {
                (" · ✕ ", RED)
            };
            heading
                .spans
                .push(Span::styled(marker, Style::default().fg(color)));
            heading.spans.push(Span::styled(
                summary.text.clone(),
                Style::default().fg(if summary.running || summary.ok {
                    FOREGROUND
                } else {
                    RED
                }),
            ));
            if !source.is_empty() {
                heading.spans.push(Span::styled(
                    format!(" · {source}"),
                    Style::default().fg(MUTED),
                ));
            }
        } else {
            if message.streaming {
                heading.spans.push(Span::styled(
                    format!(" · {}", common::STREAMING),
                    Style::default().fg(MUTED),
                ));
            }
            if message.truncated {
                heading.spans.push(Span::styled(
                    format!(" · {}", common::TRUNCATED),
                    Style::default().fg(AMBER),
                ));
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
        if message.role == Role::Assistant && state.markdown_display == MarkdownDisplay::Render {
            let document = state.render_cache.document(
                message.id,
                width,
                &text,
                render_candidates.contains(&message.id),
            );
            for anchor in &document.footnotes {
                if anchor.line >= document.lines.len()
                    || anchor.column >= width
                    || anchor.width == 0
                    || anchor.key.is_empty()
                    || anchor.key.len() > 256
                    || anchor.number == 0
                {
                    continue;
                }
                if footnotes.len() == MAX_FOOTNOTE_ANCHORS {
                    footnotes.pop_front();
                }
                footnotes.push_back(FootnotePosition {
                    location: FootnoteLocation {
                        message: message.id,
                        key: anchor.key.clone(),
                        definition: anchor.definition,
                        occurrence: anchor.occurrence,
                    },
                    number: anchor.number,
                    line: line_count.saturating_add(anchor.line),
                    column: anchor.column,
                    width: anchor.width.min(width - anchor.column),
                });
            }
            for line in &document.lines {
                append_wrapped_line(
                    line.clone(),
                    width,
                    &mut lines,
                    &mut line_count,
                    MAX_RENDER_LINES,
                );
            }
        } else {
            let style = match message.role {
                Role::Tool => Style::default().fg(CODE_FOREGROUND).bg(CODE_BACKGROUND),
                Role::Thinking => Style::default().fg(MUTED).bg(CODE_BACKGROUND),
                Role::System => Style::default().fg(MUTED),
                _ => Style::default().fg(FOREGROUND),
            };
            if message.role == Role::User {
                append_wrapped_line(
                    attachment_line(&text, style),
                    width,
                    &mut lines,
                    &mut line_count,
                    MAX_RENDER_LINES,
                );
            } else {
                append_wrapped(&text, width, style, &mut lines, &mut line_count);
            }
        }
        if message.truncated {
            append_wrapped(
                ui_text::TRUNCATED_DISPLAY,
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
    footnotes.retain(|position| position.line >= retained_start);
    state.footnote_positions = footnotes;
    if omitted > 0 {
        lines.push_front(Line::styled(
            ui_text::EARLIER_LINES_OMITTED,
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
    if let Some(target) = state.pending_footnote.take()
        && let Some(position) = state
            .footnote_positions
            .iter()
            .find(|position| position.location == target)
    {
        let target_row = position.line.saturating_sub(retained_start) + omitted;
        state.scroll = lines
            .len()
            .saturating_sub(target_row.saturating_add(usize::from(inner.height)));
    }
    state.scroll = state
        .scroll
        .min(lines.len().saturating_sub(usize::from(inner.height)));
    let end = lines.len().saturating_sub(state.scroll);
    let start = end.saturating_sub(usize::from(inner.height));
    for position in &state.footnote_positions {
        let row = position.line.saturating_sub(retained_start) + omitted;
        if row >= start && row < end {
            state.footnote_hits.push(FootnoteHit {
                area: Rect::new(
                    inner.x + position.column as u16,
                    inner.y + (row - start) as u16,
                    position.width as u16,
                    1,
                ),
                location: position.location.clone(),
            });
        }
    }
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
