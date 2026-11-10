//! Keyboard-first selector for discovered Skills, MCP servers and installed Plugins.
use super::*;

pub(super) fn draw(frame: &mut Frame<'_>, state: &mut State) {
    let Some(panel) = &mut state.picker_panel else {
        return;
    };
    let area = frame.area();
    let width = area.width.saturating_sub(4).clamp(20, 104).min(area.width);
    let height = area.height.saturating_sub(2).max(6).min(area.height);
    let modal = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(Line::styled(
            format!(" {} ", panel.title),
            Style::default()
                .fg(STRONG_FOREGROUND)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(SIDEBAR_BACKGROUND).fg(FOREGROUND))
        .border_style(Style::default().fg(BLUE));
    let inner = block.inner(modal);
    frame.render_widget(block, modal);
    let rows = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);
    panel.page_rows = usize::from(rows[0].height).max(1);
    if panel.items.is_empty() {
        panel.selected = 0;
        panel.scroll = 0;
        frame.render_widget(
            Paragraph::new(panel.hint.as_str()).style(Style::default().fg(MUTED)),
            rows[0],
        );
    } else {
        let selected = panel.selected.min(panel.items.len() - 1);
        panel.selected = selected;
        panel.scroll = panel
            .scroll
            .min(panel.items.len().saturating_sub(panel.page_rows));
        if selected < panel.scroll {
            panel.scroll = selected;
        } else if selected >= panel.scroll + panel.page_rows {
            panel.scroll = selected + 1 - panel.page_rows;
        }
        let label_width = panel
            .items
            .iter()
            .map(|item| item.label.width())
            .max()
            .unwrap_or(0)
            .min(usize::from(rows[0].width).saturating_sub(8));
        for (row, (index, item)) in panel
            .items
            .iter()
            .enumerate()
            .skip(panel.scroll)
            .take(panel.page_rows)
            .enumerate()
        {
            let active = index == selected;
            let style = if active {
                Style::default()
                    .fg(BACKGROUND)
                    .bg(BLUE)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(FOREGROUND).bg(SIDEBAR_BACKGROUND)
            };
            let label = crop_display(&item.label, 0, label_width);
            let mut line = Line::from(vec![
                Span::raw(if active { "› " } else { "  " }),
                Span::raw(if item.active { "● " } else { "○ " }),
                Span::raw(label.clone()),
            ])
            .style(style);
            let remaining = usize::from(rows[0].width).saturating_sub(4 + label_width + 2);
            if remaining >= 4 {
                line.spans.push(Span::raw(
                    " ".repeat(label_width.saturating_sub(item.label.width()) + 2),
                ));
                line.spans.push(Span::styled(
                    crop_display(&item.description, 0, remaining - 2),
                    Style::default().fg(if active { BACKGROUND } else { MUTED }),
                ));
            }
            frame.render_widget(
                line,
                Rect::new(rows[0].x, rows[0].y + row as u16, rows[0].width, 1),
            );
        }
    }
    let count = if panel.items.is_empty() {
        "0 items".into()
    } else {
        format!("{}/{}", panel.selected + 1, panel.items.len())
    };
    let action = match panel.kind {
        PickerKind::Mcps => "Enter toggle · ↑↓ select · Esc close",
        PickerKind::Skills | PickerKind::Plugins => "Enter details · ↑↓ select · Esc close",
    };
    frame.render_widget(
        Paragraph::new(format!("{count} · {action}")).style(Style::default().fg(MUTED)),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new("Selection is explicit; the unsubmitted chat draft is retained.")
            .style(Style::default().fg(MUTED)),
        rows[2],
    );
    state.transcript_viewport = None;
    state.details_viewport = Some(rows[0]);
    state.completion_viewport = None;
    state.message_hits.clear();
    state.footnote_hits.clear();
    state.palette_hits.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn selector_scrolls_to_the_active_item_and_reports_empty_state() {
        let items = (0..20)
            .map(|index| PickerItem {
                id: index.to_string(),
                label: format!("item-{index}"),
                description: "configured".into(),
                details: vec![format!("detail-{index}")],
                active: index == 19,
            })
            .collect();
        let mut state = State {
            picker_panel: Some(PickerPanel {
                kind: PickerKind::Mcps,
                title: "MCP servers".into(),
                items,
                selected: 19,
                scroll: 0,
                page_rows: 0,
                hint: "empty".into(),
            }),
            ..State::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let panel = state.picker_panel.as_ref().unwrap();
        assert!(panel.scroll > 0);
        assert!(panel.page_rows > 0);
        state.picker_panel.as_mut().unwrap().items.clear();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert_eq!(state.picker_panel.as_ref().unwrap().selected, 0);
    }
}
