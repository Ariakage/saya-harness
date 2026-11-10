//! Presentation-only memory administration modal. Input belongs to this pane,
//! so browsing and confirmation never alter the unsent chat message.
use super::*;
use crate::i18n::memory as text;

pub(super) fn draw(frame: &mut Frame<'_>, state: &mut State) {
    let Some(panel) = &mut state.memory_panel else {
        return;
    };
    let area = frame.area();
    let width = area.width.saturating_sub(4).clamp(16, 112).min(area.width);
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
        .title(text::TITLE)
        .style(Style::default().bg(SIDEBAR_BACKGROUND).fg(FOREGROUND))
        .border_style(Style::default().fg(if panel.confirmation.is_some() {
            AMBER
        } else {
            BLUE
        }));
    let inner = block.inner(modal);
    frame.render_widget(block, modal);
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(u16::from(inner.height >= 6)),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(u16::from(inner.height >= 5)),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(safe_line(&panel.settings)).style(Style::default().fg(AMBER)),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(text::HELP).style(Style::default().fg(MUTED)),
        rows[1],
    );
    let mut lines = VecDeque::new();
    let mut count = 0;
    for line in &panel.lines {
        append_wrapped_line(
            Line::raw(safe_line(line)),
            rows[2].width as usize,
            &mut lines,
            &mut count,
            10_000,
        );
        if count > 10_000 {
            lines.push_back(Line::raw(text::VIEW_TRUNCATED));
            break;
        }
    }
    panel.page_rows = usize::from(rows[2].height).max(1);
    panel.scroll = panel
        .scroll
        .min(lines.len().saturating_sub(panel.page_rows));
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .skip(panel.scroll)
                .take(panel.page_rows)
                .collect::<Vec<_>>(),
        ),
        rows[2],
    );
    let hint = if panel.busy {
        text::BUSY
    } else if panel.confirmation.is_some() {
        text::CONFIRM
    } else {
        text::COMMANDS
    };
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(if panel.confirmation.is_some() {
            AMBER
        } else {
            MUTED
        })),
        rows[3],
    );
    draw_input(frame, &panel.input, rows[4]);
    frame.render_widget(
        Paragraph::new(text::DRAFT_RETAINED).style(Style::default().fg(MUTED)),
        rows[5],
    );
    state.transcript_viewport = None;
    state.details_viewport = Some(rows[2]);
    state.completion_viewport = None;
    state.message_hits.clear();
    state.footnote_hits.clear();
    state.palette_hits.clear();
}

fn draw_input(frame: &mut Frame<'_>, editor: &Editor, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let prompt = if area.width > 1 { "> " } else { ">" };
    let prefix = prompt.len() as u16;
    let width = area.width.saturating_sub(prefix);
    let text = safe_line(editor.text());
    let before_cursor = safe_line(&editor.text()[..editor.cursor()]);
    let column = UnicodeWidthStr::width(before_cursor.as_str());
    let first = column.saturating_sub(usize::from(width.saturating_sub(1)));
    let visible = crop_display(&text, first, usize::from(width));
    frame.render_widget(
        Paragraph::new(format!("{prompt}{visible}"))
            .style(Style::default().bg(INPUT_BACKGROUND).fg(STRONG_FOREGROUND)),
        area,
    );
    let cursor = if width == 0 {
        area.width - 1
    } else {
        prefix
            + u16::try_from(column - first)
                .unwrap_or(width - 1)
                .min(width - 1)
    };
    frame.set_cursor_position((area.x + cursor, area.y));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn modal_is_bounded_on_compact_and_wide_windows_and_retains_draft() {
        for (width, height) in [(80, 24), (140, 40), (24, 8)] {
            let mut state = State::default();
            state.editor.set_text("retained message draft");
            state.memory_panel = Some(MemoryPanel {
                settings: "read true · inject true · write true".into(),
                lines: vec![
                    "Current workspace scope".into(),
                    "revision abcdef".into(),
                    "source user".into(),
                ],
                ..Default::default()
            });
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| super::super::draw(frame, &mut state))
                .unwrap();
            let screen = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(screen.contains("Memory"));
            assert_eq!(state.editor.text(), "retained message draft");
            assert!(state.transcript_viewport.is_none());
        }
    }

    #[test]
    fn wheel_and_page_rows_target_memory_without_scrolling_conversation() {
        let mut state = State {
            memory_panel: Some(MemoryPanel {
                lines: (0..100).map(|number| format!("Source {number}")).collect(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| super::super::draw(frame, &mut state))
            .unwrap();
        let area = state.details_viewport.unwrap();
        assert!(state.mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: area.x + 1,
            row: area.y + 1,
            modifiers: crossterm::event::KeyModifiers::NONE
        }));
        assert_eq!(state.memory_panel.as_ref().unwrap().scroll, 3);
        assert_eq!(state.scroll, 0);
        assert!(state.memory_panel.as_ref().unwrap().page_rows > 0);
    }

    #[test]
    fn modal_input_scrolls_long_confirmation_and_places_its_own_caret() {
        let text = format!("confirm op_{} {}", "a".repeat(32), "b".repeat(64));
        for (width, height) in [(140, 40), (80, 24), (24, 8), (20, 6)] {
            let mut state = State {
                memory_panel: Some(MemoryPanel::default()),
                ..Default::default()
            };
            state.editor.set_text("Retained chat draft");
            state.memory_panel.as_mut().unwrap().input.set_text(&text);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| super::super::draw(frame, &mut state))
                .unwrap();
            let cursor = terminal.backend().cursor_position();
            assert!(cursor.x < width && cursor.y < height);
            let line = (0..width)
                .map(|column| terminal.backend().buffer()[(column, cursor.y)].symbol())
                .collect::<String>();
            assert!(line.contains("bbbb"));
            assert_eq!(
                terminal.backend().buffer()[(cursor.x, cursor.y)].symbol(),
                " "
            );
            assert_eq!(state.memory_panel.as_ref().unwrap().input.text(), text);
            assert_eq!(state.editor.text(), "Retained chat draft");
            let panel = state.memory_panel.as_mut().unwrap();
            panel.input.home();
            terminal
                .draw(|frame| super::super::draw(frame, &mut state))
                .unwrap();
            let cursor = terminal.backend().cursor_position();
            assert_eq!(
                terminal.backend().buffer()[(cursor.x, cursor.y)].symbol(),
                "c"
            );
        }
    }

    #[test]
    fn memory_input_caret_tracks_graphemes_wide_cells_and_escaped_line_breaks() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut editor = Editor::default();
        editor.set_text("界e\u{301}👩‍💻\n中tail");
        let original = editor.text().to_owned();
        let mut terminal = Terminal::new(TestBackend::new(40, 4)).unwrap();
        let area = Rect::new(2, 1, 32, 1);
        let draw = |editor: &Editor, terminal: &mut Terminal<TestBackend>| {
            terminal
                .draw(|frame| draw_input(frame, editor, area))
                .unwrap();
            let cursor = terminal.backend().cursor_position();
            assert_eq!(cursor.y, area.y);
            assert!(cursor.x >= area.x + 2 && cursor.x < area.right());
            cursor
        };
        let end = draw(&editor, &mut terminal);
        let expected = UnicodeWidthStr::width(safe_line(&original).as_str());
        assert_eq!(usize::from(end.x - area.x - 2), expected);
        editor.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        let left = draw(&editor, &mut terminal);
        assert_eq!(left.x + 1, end.x);
        for _ in 0..4 {
            editor.move_left();
        }
        let before_wide = draw(&editor, &mut terminal);
        editor.move_left();
        let after_wide = draw(&editor, &mut terminal);
        assert_eq!(after_wide.x + 2, before_wide.x);
        editor.move_left();
        let before_emoji = draw(&editor, &mut terminal);
        assert_eq!(before_emoji.x + 2, after_wide.x);
        assert_eq!(editor.text(), original);
        editor.home();
        draw(&editor, &mut terminal);
        editor.delete();
        assert!(editor.text().is_char_boundary(editor.cursor()));
    }

    #[test]
    fn narrow_memory_input_windows_keep_unicode_clusters_and_editor_bytes() {
        let text = format!("{} tail界e\u{301}👩‍💻", "界e\u{301}👩‍💻".repeat(80));
        let mut editor = Editor::default();
        editor.set_text(&text);
        let mut terminal = Terminal::new(TestBackend::new(20, 3)).unwrap();
        for _ in 0..12 {
            terminal
                .draw(|frame| draw_input(frame, &editor, Rect::new(1, 1, 10, 1)))
                .unwrap();
            let cursor = terminal.backend().cursor_position();
            assert!(cursor.x >= 3 && cursor.x < 11 && cursor.y == 1);
            let visible = (1..11)
                .map(|column| terminal.backend().buffer()[(column, 1)].symbol())
                .collect::<String>();
            assert!(!visible.contains('\u{fffd}'));
            assert_eq!(editor.text(), text);
            editor.move_left();
        }
    }
}
