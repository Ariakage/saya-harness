use super::draw::{attachment_line, draw_split_line, safe_line};
use super::*;

pub(super) fn draw_details(
    frame: &mut Frame<'_>,
    details: &Details,
    scroll: &mut usize,
    page_rows: &mut usize,
    area: Rect,
    modal: bool,
) {
    let title = safe_line(&details.title[..prefix_boundary(&details.title, 256)]);
    let title_style = Style::default()
        .fg(
            if title.contains(ui_text::APPROVAL_REQUIRED) || title.contains(ui_text::QUESTION) {
                AMBER
            } else {
                FOREGROUND
            },
        )
        .add_modifier(Modifier::BOLD);
    let mut block = if modal {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().bg(SIDEBAR_BACKGROUND))
            .border_style(Style::default().fg(if title_style.fg == Some(AMBER) {
                AMBER
            } else {
                BLUE
            }))
    } else {
        Block::default().border_style(Style::default().fg(BORDER))
    };
    if area.height > 1 {
        if !modal {
            block = block.borders(Borders::TOP);
        }
        block = block.title(Line::styled(format!(" {title} "), title_style));
    }
    let inner = block.inner(area);
    *page_rows = usize::from(inner.height).clamp(1, if modal { 64 } else { 10 });
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
        .is_some_and(|line| line == ui_text::DETAIL_TRUNCATION_NOTICE);
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
            let prefix = crop_display_with_budget(
                line,
                0,
                usize::from(inner.width) * rows_left,
                MAX_TRANSCRIPT_BYTES,
            );
            let line_truncated = prefix.len() < line.len();
            append_wrapped(
                &prefix,
                usize::from(inner.width),
                detail_content_style(line),
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
        && !title.starts_with(ui_text::TRUNCATED_DETAIL_PREFIX)
    {
        block = block.title(Line::styled(
            format!(" {}{title} ", ui_text::TRUNCATED_DETAIL_PREFIX),
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ));
    }
    if truncated {
        if lines.len() == MAX_RENDER_LINES {
            lines.pop_back();
        }
        lines.push_back(Line::styled(
            ui_text::DETAIL_TRUNCATION_NOTICE,
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

pub(super) fn detail_content_style(line: &str) -> Style {
    if line == ui_text::DETAIL_TRUNCATION_NOTICE {
        return Style::default().fg(AMBER);
    }
    if line.starts_with(tool_text::EXECUTION_RESULT) || line.starts_with(tool_text::RESULT) {
        let color = if line.ends_with(tool_text::COMPLETED) {
            GREEN
        } else if line.ends_with(tool_text::FAILED) {
            RED
        } else {
            BLUE
        };
        return Style::default().fg(color).add_modifier(Modifier::BOLD);
    }
    if [
        tool_text::COMMAND,
        tool_text::WORKING_DIRECTORY,
        tool_text::TIMEOUT_LABEL,
        tool_text::ENVIRONMENT,
        tool_text::OBSERVED_DURATION,
        tool_text::OUTPUT,
    ]
    .contains(&line)
    {
        return Style::default().fg(BLUE).add_modifier(Modifier::BOLD);
    }
    Style::default().fg(FOREGROUND)
}

pub(super) fn draw_editor(frame: &mut Frame<'_>, state: &State, area: Rect) {
    let accent = mode_color(&state.mode);
    frame.render_widget(
        Block::default().style(Style::default().bg(BACKGROUND)),
        area,
    );
    // Keep the existing left padding and reserve a dark cell before the frame.
    let area = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if area.width == 0 || area.height == 0 {
        return;
    }
    let block = Block::default()
        .borders(Borders::LEFT)
        .style(Style::default().fg(FOREGROUND).bg(INPUT_BACKGROUND));
    let mut inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                INPUT_STRIPE,
                Style::default().fg(accent).bg(INPUT_BACKGROUND)
            );
            usize::from(area.height)
        ]),
        Rect::new(area.x, area.y, 1, area.height),
    );
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
            ui_text::PASTE_TRUNCATED
        } else {
            ui_text::MESSAGE
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
            let mut rendered =
                attachment_line(&draft, Style::default().fg(FOREGROUND).bg(INPUT_BACKGROUND));
            if row == cursor_row
                && let Some(ghost) = &ghost
            {
                rendered.spans.push(Span::styled(
                    crop_display(ghost, 0, remaining),
                    Style::default().fg(MUTED),
                ));
            }
            rendered
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), inner);
    if area.height > 2
        && state.editor.text().is_empty()
        && !state.editor.truncated
        && !state.completion.visible()
        && !matches!(
            state.status.as_str(),
            common::AWAITING_INPUT | common::AWAITING_APPROVAL | "NeedsInput" | "NeedsConfirmation"
        )
    {
        frame.render_widget(
            Line::styled(
                crop_display(
                    ui_text::DESCRIPTION_PLACEHOLDER,
                    0,
                    usize::from(inner.width),
                ),
                Style::default().fg(MUTED).add_modifier(Modifier::ITALIC),
            ),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
    }
    if state.command_palette.is_none()
        && state.picker_panel.is_none()
        && state.memory_panel.is_none()
    {
        frame.set_cursor_position((
            inner.x + (cursor_column - first_column) as u16,
            inner.y + (cursor_row - first_row) as u16,
        ));
    }
}

pub(super) fn editor_hint(width: u16) -> Line<'static> {
    if width < 38 {
        return Line::default();
    }
    let key = Style::default()
        .fg(STRONG_FOREGROUND)
        .bg(CODE_BACKGROUND)
        .add_modifier(Modifier::BOLD);
    let action = Style::default().fg(MUTED);
    let mut spans = vec![
        Span::styled(ui_text::ENTER_SEND, key),
        Span::styled(ui_text::ENTER_ACTION, action),
    ];
    if width >= 64 {
        spans.push(Span::styled(ui_text::NEWLINE_KEY, key));
        spans.push(Span::styled(ui_text::NEWLINE_ACTION, action));
    }
    spans.push(Span::styled(ui_text::FOLD_KEY, key));
    spans.push(Span::styled(ui_text::FOLD_ACTION, action));
    Line::from(spans)
}

pub(super) fn draw_command_palette(frame: &mut Frame<'_>, state: &mut State) {
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
            format!(" {} ", ui_text::COMMANDS),
            Style::default()
                .fg(STRONG_FOREGROUND)
                .add_modifier(Modifier::BOLD),
        ));
    if compact {
        block = block.title_bottom(Line::styled(ui_text::ESC_CLOSE, Style::default().fg(MUTED)));
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
                ui_text::PALETTE_NAV
            } else {
                ui_text::PALETTE_FULL_NAV
            },
            Style::default().fg(MUTED),
        ),
        Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
    );
}

pub(super) fn draw_completion(frame: &mut Frame<'_>, state: &mut State, area: Rect) {
    if state.command_palette.is_some()
        || state.memory_panel.is_some()
        || state.picker_panel.is_some()
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
        .title(format!(" {} ", ui_text::COMMAND_SUGGESTIONS))
        .title_bottom(ui_text::PALETTE_COMPLETION_NAV)
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

pub(super) fn crop_display(text: &str, skip: usize, width: usize) -> String {
    crop_display_with_budget(text, skip, width, MAX_VISUAL_LINE_BYTES)
}

pub(super) fn crop_display_with_budget(
    text: &str,
    skip: usize,
    width: usize,
    byte_limit: usize,
) -> String {
    let mut result = String::new();
    let mut column = 0;
    let mut retained = 0;
    for grapheme in text.graphemes(true) {
        let grapheme = display_grapheme(grapheme);
        let cells = UnicodeWidthStr::width(grapheme);
        let end = column + cells;
        if end > skip {
            let visible = cells.min(end - skip);
            if retained + visible > width {
                break;
            }
            if result.len().saturating_add(grapheme.len()) > byte_limit {
                let mut end = 0;
                let mut cells = 0;
                for (index, grapheme) in result.grapheme_indices(true) {
                    cells += UnicodeWidthStr::width(grapheme);
                    if index + grapheme.len() > byte_limit.saturating_sub("…".len())
                        || cells > width.saturating_sub(1)
                    {
                        break;
                    }
                    end = index + grapheme.len();
                }
                result.truncate(end);
                if width > 0 && byte_limit >= "…".len() {
                    result.push('…');
                }
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

pub(super) fn append_wrapped(
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
