use super::draw::*;
use super::overlays::*;
use super::*;
use crate::{
    commands::Suggestion,
    theme::{HEADING, INLINE_CODE},
};
use ratatui::{
    Terminal,
    backend::{Backend, TestBackend},
    style::Color,
};

fn render(state: &mut State, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn row_text(buffer: &ratatui::buffer::Buffer, row: u16) -> String {
    (0..buffer.area.width)
        .map(|column| buffer[(column, row)].symbol())
        .collect()
}

#[test]
fn copied_notice_uses_the_top_right_status_badge() {
    let mut state = State {
        copied_notice: true,
        ..State::default()
    };
    let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer();
    let row = row_text(buffer, 0);
    assert!(row.contains(common::COPIED));
    let column = row.find('C').unwrap() as u16;
    assert_eq!(buffer[(column, 0)].bg, GREEN);
    assert_eq!(buffer[(column, 0)].fg, BACKGROUND);
    state.copied_notice = false;
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    assert!(!row_text(terminal.backend().buffer(), 0).contains(common::COPIED));
}

#[test]
fn split_utf8_streams_join_and_viewport_budget_is_explicit() {
    let mut state = State::default();
    let id = state.push_message(Role::Assistant, "Soyo", "");
    for delta in ["hello ", "界", "👩‍💻", " e", "\u{301}"] {
        assert!(state.append_message(id, delta));
    }
    state.finish_message(id);
    assert_eq!(state.messages.back().unwrap().text, "hello 界👩‍💻 e\u{301}");
    for _ in 0..12 {
        state.push_message(Role::Tool, "Tool", "界".repeat(MAX_MESSAGE_BYTES));
    }
    assert!(
        state
            .messages
            .iter()
            .map(|message| message.text.len() + message.label.len())
            .sum::<usize>()
            <= MAX_TRANSCRIPT_BYTES
    );
    assert!(state.dropped_messages > 0);
    assert!(
        state
            .messages
            .iter()
            .all(|message| message.text.is_char_boundary(message.text.len()))
    );
    let collapsed = render(&mut state, 80, 24);
    assert!(collapsed.contains("truncated"));
    assert!(!collapsed.contains("display truncated"));
    assert!(state.toggle_latest());
    state.scroll_to_bottom();
    assert!(render(&mut state, 80, 24).contains("display truncated"));
}

#[test]
fn canonical_replacement_and_session_reset_keep_stable_ids_and_budgets() {
    let mut state = State::default();
    let first = state.push_message(Role::Assistant, "Soyo", "partial");
    state.append_message(first, " text");
    assert!(state.replace_message(first, "canonical answer"));
    assert_eq!(state.messages[0].text, "canonical answer");
    assert!(!state.messages[0].streaming);
    state.scroll_up(10);
    state.clear_messages();
    assert!(state.messages.is_empty());
    assert_eq!(state.scroll, 0);
    let next = state.push_message(Role::User, "You", "new session");
    assert!(next > first);
    assert!(!state.replace_message(first, "old result"));
}

#[test]
fn narrow_frames_multiline_cursor_and_controls_render_without_escape_effects() {
    let mut state = State::default();
    state.push_message(
        Role::User,
        "You",
        "line\x1b[2J\u{9b}\u{202e}\n```rust\nlet x = 1;\n```",
    );
    state.editor.insert("hello\n界🌸\nlast");
    state.editor.move_up();
    let image = render(&mut state, 80, 24);
    assert!(image.contains("\\u{1b}"));
    assert!(!image.contains('\x1b'));
    assert!(!image.contains('\u{9b}'));
    for (width, height) in [(0, 0), (1, 1), (19, 5), (20, 6), (30, 10), (120, 30)] {
        render(&mut state, width, height);
    }
}

#[test]
fn rendered_cells_bound_combining_clusters_in_transcript_details_and_editor() {
    let source = format!("a{} tail", "\u{301}".repeat(100_000));
    let mut state = State::default();
    state.push_message(Role::Assistant, "Soyo", source.clone());
    state.editor.set_text(&source);
    for details in [false, true] {
        if details {
            state.set_details(Details {
                title: "Parameters".into(),
                lines: vec![source.clone()],
            });
        }
        for (width, height) in [(20, 6), (80, 24), (120, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let cells = &terminal.backend().buffer().content;
            assert!(
                cells
                    .iter()
                    .all(|cell| cell.symbol().len() <= crate::editor::MAX_GRAPHEME_BYTES)
            );
            assert!(cells.iter().any(|cell| cell.symbol() == "…"));
            for row in cells.chunks(usize::from(width)) {
                assert!(
                    row.iter().map(|cell| cell.symbol().len()).sum::<usize>()
                        <= MAX_VISUAL_LINE_BYTES
                );
            }
        }
    }
    assert_eq!(state.messages[0].text, source);
    assert!(state.editor.text().starts_with('a'));
    let zeros = format!("x{}tail", "\u{200b}".repeat(100_000));
    let cropped = crop_display(&zeros, 0, 80);
    assert!(cropped.len() <= MAX_VISUAL_LINE_BYTES);
    assert!(cropped.ends_with('…'));
    assert_eq!(crop_display(&source, 0, 80), "… tail");
}

#[test]
fn details_scroll_reaches_retained_long_fields_and_declares_display_limits() {
    let mut state = State {
        details: Some(Details {
            title: "Parameters".into(),
            lines: vec![
                (0..100)
                    .map(|number| format!("row {number}\n"))
                    .collect::<String>(),
            ],
        }),
        ..State::default()
    };
    assert!(render(&mut state, 60, 20).contains("row 0"));
    state.details_scroll = 90;
    assert!(render(&mut state, 60, 20).contains("row 99"));
    state.details = Some(Details {
        title: "Oversized".into(),
        lines: vec!["x".repeat(MAX_TRANSCRIPT_BYTES + 1)],
    });
    state.details_scroll = 0;
    assert!(render(&mut state, 60, 20).contains("Truncated"));
}

#[test]
fn details_setter_bounds_owned_bytes_rows_and_allocation_without_changing_scroll() {
    let mut state = State {
        details_scroll: 17,
        ..State::default()
    };
    state.set_details(Details {
        title: "Parameters".into(),
        lines: vec!["界".repeat(12 * 1024 * 1024)],
    });
    let details = state.details.as_ref().unwrap();
    assert_eq!(details.title, "Parameters");
    assert_eq!(
        details.lines.last().unwrap(),
        ui_text::DETAIL_TRUNCATION_NOTICE
    );
    assert!(details.lines.iter().map(String::len).sum::<usize>() <= MAX_TRANSCRIPT_BYTES);
    assert!(details.lines.iter().map(String::capacity).sum::<usize>() <= MAX_TRANSCRIPT_BYTES);
    assert!(details.lines.len() <= MAX_DETAIL_LINES);
    assert_eq!(state.details_scroll, 17);
    assert!(render(&mut state, 60, 20).contains("Truncated"));
    state.set_details(Details {
        title: "Rows".into(),
        lines: vec!["line\n".repeat(MAX_DETAIL_LINES + 20)],
    });
    let details = state.details.as_ref().unwrap();
    assert_eq!(details.lines.len(), MAX_DETAIL_LINES);
    assert_eq!(
        details.lines.last().unwrap(),
        ui_text::DETAIL_TRUNCATION_NOTICE
    );
    assert!(details.lines.capacity() <= MAX_DETAIL_LINES);
}

#[test]
fn draw_normalizes_direct_details_even_on_tiny_frames_and_is_idempotent() {
    let mut state = State {
        details: Some(Details {
            title: "Direct assignment".into(),
            lines: vec!["x".repeat(MAX_TRANSCRIPT_BYTES + 1)],
        }),
        ..State::default()
    };
    render(&mut state, 1, 1);
    let details = state.details.as_ref().unwrap();
    assert!(details.lines.iter().map(String::capacity).sum::<usize>() <= MAX_TRANSCRIPT_BYTES);
    let title = details.title.clone();
    let lines = details.lines.clone();
    render(&mut state, 60, 20);
    assert_eq!(state.details.as_ref().unwrap().title, title);
    assert_eq!(state.details.as_ref().unwrap().lines, lines);
}

#[test]
fn unused_details_capacity_is_released_without_claiming_data_truncation() {
    let mut source = String::with_capacity(MAX_TRANSCRIPT_BYTES * 4);
    source.push_str("retained");
    let mut state = State::default();
    state.set_details(Details {
        title: "Tool".into(),
        lines: vec![source],
    });
    let details = state.details.as_ref().unwrap();
    assert_eq!(details.title, "Tool");
    assert_eq!(details.lines, ["retained"]);
    assert_eq!(details.lines[0].capacity(), "retained".len());
}

#[test]
fn scrollback_stays_anchored_while_new_lines_arrive_and_details_adapt() {
    let mut state = State::default();
    let id = state.push_message(
        Role::Assistant,
        "Soyo",
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten",
    );
    render(&mut state, 40, 12);
    state.scroll_up(3);
    render(&mut state, 40, 12);
    let previous = state.scroll;
    state.append_message(id, "\neleven\ntwelve");
    render(&mut state, 40, 12);
    assert_eq!(state.scroll, previous + 2);
    state.details = Some(Details {
        title: "Tool details".into(),
        lines: vec!["call_id: fixture".into(), "state: unknown".into()],
    });
    assert!(render(&mut state, 120, 24).contains("Tool details"));
    assert!(render(&mut state, 40, 12).contains("unknown"));
}

#[test]
fn completion_ghost_keeps_the_draft_cursor_and_details_unchanged() {
    let mut state = State::default();
    state.editor.set_text("/per");
    state
        .completion
        .update(state.editor.text(), state.editor.cursor(), true);
    state.set_details(Details {
        title: "Saved records".into(),
        lines: vec!["state unknown".into()],
    });
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let cursor = terminal.backend_mut().get_cursor_position().unwrap();
    assert_eq!((cursor.x, cursor.y), (7, 20));
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(cursor.x, cursor.y)].symbol(), "m");
    assert_eq!(buffer[(cursor.x, cursor.y)].fg, MUTED);
    assert_eq!(state.editor.text(), "/per");
    assert_eq!(state.editor.cursor(), 4);
    assert_eq!(state.details.as_ref().unwrap().title, "Saved records");
    assert_eq!(state.details.as_ref().unwrap().lines, ["state unknown"]);
    assert!(
        buffer
            .content
            .iter()
            .any(|cell| { cell.symbol() == "›" && cell.bg == BLUE })
    );
    let image = render(&mut state, 120, 24);
    assert!(image.contains("Command suggestions"));
    assert!(image.contains("Tab/→ Esc"));
}

#[test]
fn dismissed_completion_clears_its_popup_and_ghost_on_the_same_terminal() {
    let mut state = State::default();
    state.editor.set_text("/per");
    state
        .completion
        .update(state.editor.text(), state.editor.cursor(), true);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    state.completion.dismiss();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let image = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(!image.contains("Command suggestions"));
    assert!(!image.contains('›'));
    assert!(!image.contains("/permissions"));
    assert_eq!(terminal.backend().buffer()[(7, 20)].symbol(), " ");
    assert_eq!(state.editor.text(), "/per");
}

#[test]
fn narrow_completion_keeps_the_selected_row_in_its_six_row_window() {
    let mut state = State::default();
    state.editor.set_text("/");
    state.completion.update("/", 1, true);
    assert!(state.completion.items.len() > MAX_COMPLETION_ROWS);
    state.completion.selected = state
        .completion
        .items
        .iter()
        .rposition(|item| item.text == "/export-full markdown ")
        .unwrap();
    let selected = state.completion.current().unwrap().text;
    let mut terminal = Terminal::new(TestBackend::new(26, 16)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer();
    let row = (0..16)
        .find(|row| buffer[(3, *row)].symbol() == "›")
        .unwrap();
    assert_eq!(row, 8);
    assert_eq!(buffer[(3, row)].bg, BLUE);
    let actual = (5..23)
        .map(|column| buffer[(column, row)].symbol())
        .collect::<String>();
    assert_eq!(actual.trim_end(), crop_display(selected, 0, 18));
    assert_eq!(
        buffer
            .content
            .iter()
            .filter(|cell| cell.symbol() == "›")
            .count(),
        1
    );
    assert_eq!(state.editor.text(), "/");
}

#[test]
fn ghost_respects_unicode_horizontal_cropping_and_end_of_draft() {
    let mut state = State::default();
    state.editor.set_text("/界界界");
    state.completion.update("/", 1, true);
    state.completion.items = vec![Suggestion {
        text: "/界界界ab",
        description: "Unicode preview",
    }];
    let mut terminal = Terminal::new(TestBackend::new(6, 3)).unwrap();
    terminal
        .draw(|frame| draw_editor(frame, &state, frame.area()))
        .unwrap();
    let cursor = terminal.backend_mut().get_cursor_position().unwrap();
    assert_eq!((cursor.x, cursor.y), (4, 1));
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(2, 1)].symbol(), "界");
    assert_eq!(buffer[(3, 1)].symbol(), " ");
    assert_eq!(buffer[(4, 1)].symbol(), "a");
    assert_eq!(buffer[(4, 1)].fg, MUTED);
    assert_eq!(buffer[(5, 1)].bg, BACKGROUND);
    assert_eq!(state.editor.text(), "/界界界");
    state.editor.move_left();
    terminal
        .draw(|frame| draw_editor(frame, &state, frame.area()))
        .unwrap();
    assert!(
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .all(|cell| { cell.symbol() != "a" && cell.symbol() != "b" })
    );
}

#[test]
fn completion_degrades_safely_on_small_frames_with_bounded_history() {
    let mut state = State::default();
    for _ in 0..12 {
        state.push_message(Role::Tool, "Tool", "界".repeat(MAX_MESSAGE_BYTES / 3));
    }
    state.editor.set_text("/per");
    state
        .completion
        .update(state.editor.text(), state.editor.cursor(), true);
    for (width, height) in [(0, 0), (1, 1), (19, 5), (20, 6), (30, 10), (120, 30)] {
        render(&mut state, width, height);
        assert_eq!(state.editor.text(), "/per");
    }
    let image = render(&mut state, 20, 6);
    assert!(image.contains("/permissions"));
    assert!(!image.contains('›'));
    assert!(state.dropped_messages > 0);
    assert!(state.messages.len() <= MAX_MESSAGES);
}

#[test]
fn session_layout_keeps_status_input_and_footer_in_their_observable_regions() {
    let mut state = State {
        workspace: "/tmp/project".into(),
        model: "configured-model".into(),
        context_window_tokens: Some(128_000),
        context_input_tokens: Some(64_000),
        ..State::default()
    };
    state.editor.set_text("hello");
    for (width, height, status_row, title_row, input_row, main_width) in [
        (80, 24, 18, 19, 20, 80),
        (60, 18, 12, 13, 14, 60),
        (20, 6, 2, 3, 4, 20),
        (110, 32, 26, 27, 28, 110),
        (120, 40, 34, 35, 36, 90),
        (160, 45, 39, 40, 41, 120),
    ] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        assert_eq!((cursor.x, cursor.y), (8, input_row));
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].bg, BACKGROUND);
        assert!(row_text(buffer, 0).contains("Soyo Harness"));
        assert!(row_text(buffer, 0).contains("Ctrl+P"));
        assert!(row_text(buffer, 1).contains("/tmp/project"));
        assert!(row_text(buffer, status_row).contains("✓ Ready"));
        let status = (0..main_width)
            .map(|column| buffer[(column, status_row)].symbol())
            .collect::<String>();
        assert!(status.ends_with("auto│"));
        assert_eq!(buffer[(3, title_row)].symbol(), "M");
        assert_eq!(buffer[(1, title_row)].symbol(), INPUT_STRIPE);
        assert_eq!(buffer[(1, title_row)].fg, mode_color("WORK"));
        assert_eq!(buffer[(1, title_row)].bg, INPUT_BACKGROUND);
        assert_eq!(buffer[(2, title_row)].bg, INPUT_BACKGROUND);
        assert_eq!(buffer[(3, input_row)].symbol(), "h");
        assert_eq!(buffer[(3, input_row)].bg, INPUT_BACKGROUND);
        let footer = row_text(buffer, height - 1);
        assert!(footer.starts_with("└WORK"));
        assert!(footer.contains("input"));
        assert!(footer.contains("50%"));
        assert!(footer.contains('◑'));
        if width >= 44 {
            assert!(row_text(buffer, 0).contains(env!("CARGO_PKG_VERSION")));
            assert!(footer.contains("64k/128k"));
        }
        assert_eq!(state.editor.text(), "hello");
    }
}

#[test]
fn resizing_across_former_breakpoints_keeps_one_reference_layout() {
    let mut state = State {
        usage: "wide-usage-marker".into(),
        ..State::default()
    };
    let mut terminal = Terminal::new(TestBackend::new(99, 23)).unwrap();
    for (width, height) in [
        (99, 23),
        (100, 23),
        (100, 24),
        (99, 24),
        (110, 32),
        (120, 40),
        (160, 45),
        (100, 24),
        (99, 23),
    ] {
        terminal.backend_mut().resize(width, height);
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        let image = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(row_text(buffer, 0).contains("Soyo Harness"));
        assert!(row_text(buffer, 0).contains(env!("CARGO_PKG_VERSION")));
        assert!(row_text(buffer, 1).starts_with("┌ • . "));
        assert!(row_text(buffer, height - 6).contains("✓ Ready"));
        assert_eq!(buffer[(3, height - 5)].symbol(), "M");
        assert_eq!(buffer[(1, height - 5)].fg, mode_color("WORK"));
        assert_eq!(buffer[(3, height - 4)].bg, INPUT_BACKGROUND);
        assert!(row_text(buffer, height - 1).starts_with("└WORK"));
        assert!(row_text(buffer, height - 1).contains("input"));
        assert!(!image.contains("SAYA"));
        assert!(!image.contains("wide-usage-marker"));
        assert!(!image.contains("Conversation"));
        assert!(row_text(buffer, height - 5).contains("Enter send"));
    }
}

#[test]
fn input_meter_distinguishes_unknown_estimated_and_reported_input_without_overflow() {
    let mut state = State::default();
    assert_eq!(state.session_title, "New session");
    assert_eq!(context_meter(&state, false), "○ input —/— (—)");
    state.context_window_tokens = Some(128_000);
    assert_eq!(context_meter(&state, false), "○ input —/128k (—)");
    state.context_input_tokens = Some(64_000);
    state.context_estimated = true;
    assert_eq!(context_meter(&state, false), "◑ input ~64k/128k (50%)");
    assert_eq!(context_meter(&state, true), "◑ input ~50%");
    state.context_estimated = false;
    state.context_window_tokens = Some(100);
    for (input, ring) in [(0, '○'), (25, '◔'), (50, '◑'), (75, '◕'), (100, '●')] {
        state.context_input_tokens = Some(input);
        assert!(context_meter(&state, false).starts_with(ring));
    }
    state.context_input_tokens = Some(u64::MAX);
    state.context_window_tokens = Some(1);
    assert!(context_meter(&state, false).ends_with("(>100%)"));
    assert_eq!(context_meter(&state, true), "● input >100%");
    state.context_window_tokens = Some(0);
    assert!(context_meter(&state, false).ends_with("/— (—)"));
    assert_eq!(context_meter(&state, true), "○ input —");
}

#[test]
fn sidebar_distinguishes_pending_execution_usage_from_pre_request_unknown() {
    let mut state = State::default();
    assert!(render(&mut state, 120, 30).contains("Unknown before request"));
    state.status = "Thinking".into();
    assert!(render(&mut state, 120, 30).contains("Awaiting execution usage"));
}

#[test]
fn multiline_editor_scrolls_within_its_gray_input_and_keeps_safety_markers() {
    let mut state = State {
        status: "Awaiting approval".into(),
        permissions: "Manual Approval".into(),
        workspace: "project\x1b[2J\u{202e}".into(),
        thinking_effort: Some("high".into()),
        ..State::default()
    };
    state
        .editor
        .set_text("one\ntwo\nthree\nfour\nfive\ne\u{301}界👩‍💻");
    let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let cursor = terminal.backend_mut().get_cursor_position().unwrap();
    assert_eq!(cursor.y, 15);
    assert!(cursor.x < 59);
    let buffer = terminal.backend().buffer();
    assert!(row_text(buffer, 10).contains("Awaiting approval"));
    assert!(row_text(buffer, 10).ends_with("Manual Approval│"));
    assert_eq!(buffer[(3, 11)].symbol(), "M");
    assert!(row_text(buffer, 13).contains("four"));
    assert!(row_text(buffer, 14).contains("five"));
    assert!(row_text(buffer, 15).contains("e\u{301}界"));
    assert_eq!(buffer[(3, 16)].bg, INPUT_BACKGROUND);
    assert!(row_text(buffer, 17).contains("high"));
    let image = render(&mut state, 60, 18);
    assert!(image.contains("\\u{1b}"));
    assert!(!image.contains('\x1b'));
    assert!(!image.contains('\u{202e}'));
    state.editor.clear();
    let tiny = render(&mut state, 20, 6);
    assert!(tiny.contains("Approval"));
    assert!(tiny.contains("manual"));
}

#[test]
fn selected_effort_remains_visible_when_model_or_workspace_names_are_long() {
    let mut state = State {
        workspace: "/long-workspace".repeat(12),
        model: "long-model-name".repeat(12),
        thinking_effort: Some("max".into()),
        context_window_tokens: Some(128_000),
        context_input_tokens: Some(64_000),
        ..State::default()
    };
    for (width, height) in [(80, 24), (60, 18), (110, 32), (120, 40)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        let footer = row_text(buffer, height - 1);
        assert!(footer.contains(" max"));
        assert!(footer.contains("input 64k/128k (50%)"));
        assert!(
            buffer
                .content
                .iter()
                .any(|cell| cell.symbol() == "m" && cell.fg == AMBER)
        );
    }
    state.thinking_effort = None;
    assert!(!render(&mut state, 110, 32).contains(" max"));
}

#[test]
fn wide_details_and_completion_share_the_session_body_without_covering_the_editor() {
    let mut state = State::default();
    state.push_message(Role::User, "You", "conversation marker");
    state.set_details(Details {
        title: "Help".into(),
        lines: vec!["operation marker".into()],
    });
    state.editor.set_text("/per");
    state.completion.update("/per", 4, true);
    for (width, main_width) in [(99, 99), (100, 100), (110, 110), (120, 90), (160, 120)] {
        let mut terminal = Terminal::new(TestBackend::new(width, 32)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        assert_eq!((cursor.x, cursor.y), (7, 28));
        let buffer = terminal.backend().buffer();
        let right_width = ((main_width - 2) / 3).clamp(32, 48);
        let details_x = if main_width >= 100 {
            main_width - 1 - right_width
        } else {
            1
        };
        let heading = (details_x..main_width - 1)
            .map(|column| buffer[(column, 2)].symbol())
            .collect::<String>();
        assert!(heading.contains("Help"));
        assert!(row_text(buffer, 3).contains("operation marker"));
        if main_width >= 100 {
            assert!(row_text(buffer, 2).contains("You"));
            assert!(row_text(buffer, 3).contains("conversation marker"));
        }
        assert!(
            buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .contains("Command suggestions")
        );
        assert_eq!(buffer[(cursor.x, cursor.y)].symbol(), "m");
        assert_eq!(buffer[(cursor.x, cursor.y)].bg, INPUT_BACKGROUND);
        assert_eq!(state.editor.text(), "/per");
        assert_eq!(state.details.as_ref().unwrap().title, "Help");
    }
}

#[test]
fn approval_and_question_details_use_centered_modals_inside_the_transcript() {
    for (title, marker) in [
        ("Approval required", "y approve · n / Esc deny"),
        ("Question", "Type a reply and press Enter"),
    ] {
        let mut state = State {
            status: if title == "Question" {
                "Awaiting input".into()
            } else {
                "Awaiting approval".into()
            },
            ..State::default()
        };
        state.push_message(Role::System, "System", "transcript marker");
        state.editor.set_text("retained draft");
        state.set_details(Details {
            title: title.into(),
            lines: vec![
                "Command".into(),
                "  /usr/bin/swift .tmp_vision.swift".into(),
                String::new(),
                marker.into(),
            ],
        });
        let image = render(&mut state, 120, 36);
        let transcript = state.transcript_viewport.unwrap();
        let modal = state.details_viewport.unwrap();
        assert!(image.contains(title));
        assert!(image.contains(marker));
        assert!(modal.x >= transcript.x && modal.y >= transcript.y);
        assert!(modal.right() <= transcript.right());
        assert!(modal.bottom() <= transcript.bottom());
        assert!(modal.x > transcript.x);
        assert!(modal.y > transcript.y);
        assert!(modal.right() < transcript.right());
        assert!(modal.bottom() < transcript.bottom());
        assert_eq!(state.editor.text(), "retained draft");
    }
}

#[test]
fn sidebar_is_responsive_and_labels_unknown_extensions_without_invented_state() {
    let mut state = State {
        session_title: "Sidebar session".into(),
        workspace: "/tmp/workspace".into(),
        context_window_tokens: Some(128_000),
        ..State::default()
    };
    let mut terminal = Terminal::new(TestBackend::new(119, 40)).unwrap();
    for (width, height, main_width) in [
        (119, 40, 119),
        (120, 40, 90),
        (120, 23, 120),
        (120, 24, 90),
        (160, 45, 120),
        (110, 32, 110),
        (119, 24, 119),
    ] {
        terminal.backend_mut().resize(width, height);
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(main_width - 1, height - 1)].symbol(), "┘");
        assert!(row_text(buffer, 0).starts_with("Soyo Harness"));
        if main_width < width {
            assert_eq!(buffer[(main_width, 0)].bg, SIDEBAR_BACKGROUND);
            for (row, text) in [
                (0, "Sidebar session"),
                (2, "Context · Unknown"),
                (3, "Window 128k"),
                (4, "Unknown before request"),
                (15, "Extensions"),
                (16, "MCP  No servers"),
                (17, "Plugins  Not inspected"),
                (18, "Skills —"),
                (height - 1, "/tmp/workspace"),
            ] {
                let actual = (main_width..width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>();
                assert!(
                    actual.contains(text),
                    "{width}x{height} row {row}: {actual}"
                );
            }
        } else {
            assert_eq!(buffer[(width - 1, 0)].bg, BACKGROUND);
            assert!(!row_text(buffer, 2).contains("Context"));
        }
    }
}

#[test]
fn sidebar_context_rows_are_bounded_safe_and_keep_small_proportions_visible() {
    let mut state = State {
        context_window_tokens: Some(128_000),
        context_breakdown: vec![
            ContextEntry {
                label: "Skills".into(),
                tokens: 128,
            },
            ContextEntry {
                label: "Conversation".into(),
                tokens: 64_000,
            },
        ],
        skills_count: Some(3),
        ..State::default()
    };
    let lines = context_rows(&state, 26);
    assert!(lines[0].to_string().contains("0.1%"));
    assert!(lines[0].to_string().contains('▏'));
    assert!(lines[1].to_string().contains("50%"));
    assert!(lines.iter().all(|line| line.width() <= 26));
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(row_text(buffer, 2).contains("Context · Estimated"));
    assert!(row_text(buffer, 13).contains("Total ~64.1k 50.1%"));
    assert!(row_text(buffer, 18).contains("Skills 3 discovered"));
    state.context_breakdown = (0..MAX_CONTEXT_ENTRIES + 10)
        .map(|_| ContextEntry {
            label: "unsafe\x1b[2J\u{202e}".into(),
            tokens: u64::MAX,
        })
        .collect();
    assert_eq!(context_rows(&state, 26).len(), MAX_CONTEXT_ENTRIES);
    assert!(
        context_rows(&state, 26)
            .iter()
            .all(|line| line.width() <= 26)
    );
    let image = render(&mut state, 120, 40);
    assert!(image.contains("Additional entries omitted"));
    assert!(!image.contains('\x1b'));
    assert!(!image.contains('\u{202e}'));
}

#[test]
fn context_bars_and_percentages_handle_fractional_zero_unknown_and_overflow_inputs() {
    for (tokens, expected) in [
        (0, "0%"),
        (1, "<0.1%"),
        (128, "0.1%"),
        (64_000, "50%"),
        (128_000, "100%"),
    ] {
        assert_eq!(sidebar_percentage(tokens, Some(128_000)), expected);
    }
    assert_eq!(sidebar_percentage(u64::MAX, Some(0)), "—");
    assert_eq!(sidebar_percentage(u64::MAX, Some(1)), ">100%");
    assert_eq!(
        context_bar(128, Some(128_000), 5),
        ("▏".into(), "░░░░".into())
    );
    assert_eq!(
        context_bar(64_000, Some(128_000), 5),
        ("██▌".into(), "░░".into())
    );
    assert_eq!(
        context_bar(0, Some(128_000), 5),
        (String::new(), "░░░░░".into())
    );
    assert_eq!(
        context_bar(u64::MAX, Some(0), 5),
        (String::new(), "─────".into())
    );
    assert_eq!(
        context_bar(u64::MAX, Some(1), 5),
        ("█████".into(), String::new())
    );
    assert_eq!(context_bar(1, Some(1), 0), (String::new(), String::new()));
}

#[test]
fn tiny_details_show_scrollable_content_while_retaining_an_editable_draft() {
    let mut state = State {
        status: "Awaiting approval".into(),
        permissions: "Manual Approval".into(),
        ..State::default()
    };
    state.editor.set_text("yes");
    state.set_details(Details {
        title: "Help".into(),
        lines: vec![
            "Tool read_file".into(),
            "Call c1".into(),
            "{\"path\":\"ok\"}".into(),
        ],
    });
    for height in [6, 7] {
        state.details_scroll = 0;
        let mut terminal = Terminal::new(TestBackend::new(20, height)).unwrap();
        for expected in ["Tool read_file", "Call c1", "{\"path\":\"ok\"}"] {
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let cursor = terminal.backend_mut().get_cursor_position().unwrap();
            assert_eq!((cursor.x, cursor.y), (6, height - 2));
            let buffer = terminal.backend().buffer();
            assert!(row_text(buffer, 2).contains(expected));
            assert_eq!(buffer[(3, height - 2)].symbol(), "y");
            assert_eq!(buffer[(3, height - 2)].bg, INPUT_BACKGROUND);
            assert_eq!(state.details_page_rows, 1);
            state.details_scroll += state.details_page_rows;
        }
    }
}

#[test]
fn message_headers_separate_role_metadata_and_literal_tool_cards() {
    let mut state = State::default();
    state.push_message(Role::User, "You", "**plain user text**");
    state.push_message(Role::Tool, "read_file", "`literal`\n# raw heading");
    state.push_message(Role::Assistant, "example-model", "answer");
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    assert!(row_text(buffer, 0).starts_with(" You"));
    assert_eq!(buffer[(1, 0)].fg, BLUE);
    assert!(buffer[(1, 0)].modifier.contains(Modifier::BOLD));
    assert!(row_text(buffer, 1).contains("**plain user text**"));
    assert!(row_text(buffer, 3).contains("Tool · read_file"));
    assert_eq!(buffer[(1, 3)].fg, AMBER);
    assert_eq!(buffer[(8, 3)].fg, MUTED);
    for row in 3..=5 {
        assert_eq!(buffer[(1, row)].bg, CODE_BACKGROUND);
        assert_eq!(buffer[(78, row)].bg, CODE_BACKGROUND);
    }
    assert!(row_text(buffer, 4).contains("`literal`"));
    assert!(row_text(buffer, 5).contains("# raw heading"));
    assert!(row_text(buffer, 7).contains("Soyo · example-model"));
    assert_eq!(buffer[(1, 7)].fg, FOREGROUND);
    assert_eq!(buffer[(8, 7)].fg, MUTED);
    assert_eq!(state.messages[0].text, "**plain user text**");
    assert_eq!(state.messages[1].text, "`literal`\n# raw heading");
}

#[test]
fn pasted_image_and_file_tokens_are_highlighted_in_transcript_and_editor() {
    let mut state = State::default();
    state.push_message(Role::User, "You", "[Image 1] [Audio 1] [Video 1] [File 1]");
    state
        .editor
        .set_text("Inspect [Image 1], [Audio 1], [Video 1], and [File 1]");
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer();
    for (token, color) in [
        ("[Image 1]", BLUE),
        ("[Audio 1]", AMBER),
        ("[Video 1]", HEADING),
        ("[File 1]", GREEN),
    ] {
        let mut found = 0;
        for row in 0..30 {
            let text = row_text(buffer, row);
            if let Some(byte) = text.find(token) {
                let column = text[..byte].chars().count() as u16;
                assert_eq!(buffer[(column, row)].fg, color);
                found += 1;
            }
        }
        assert_eq!(found, 2, "{token}");
    }
}

#[test]
fn assistant_markdown_uses_theme_and_preserves_source_and_code_card_width() {
    let source = "## Heading\nUse `input` and **care**.\n> quoted\n- item\n```rust\nlet x = \"**literal**\";\n```\nunsafe\x1b[2J";
    let mut state = State::default();
    state.push_message(Role::Assistant, "Soyo", source);
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    assert!(row_text(buffer, 1).starts_with(" Heading"));
    assert_eq!(buffer[(1, 1)].fg, HEADING);
    assert!(buffer[(1, 1)].modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(5, 2)].fg, INLINE_CODE);
    assert_eq!(buffer[(15, 2)].fg, AMBER);
    assert_eq!(buffer[(1, 3)].symbol(), "│");
    assert_eq!(buffer[(1, 3)].fg, MUTED);
    assert_eq!(buffer[(1, 4)].fg, AMBER);
    for row in 5..=7 {
        assert_eq!(buffer[(78, row)].bg, CODE_BACKGROUND);
    }
    assert!(row_text(buffer, 6).contains("**literal**"));
    let syntax_colors = (1..25)
        .filter(|column| buffer[(*column, 6)].symbol() != " ")
        .map(|column| buffer[(column, 6)].fg)
        .collect::<HashSet<_>>();
    assert!(syntax_colors.len() >= 2);
    assert!(syntax_colors.iter().any(|color| *color != CODE_FOREGROUND));
    assert!((0..20).any(|row| row_text(buffer, row).contains("\\u{1b}[2J")));
    assert_eq!(state.messages[0].text, source);
    assert!(state.last_line_count < MAX_RENDER_LINES);
}

#[test]
fn markdown_source_mode_preserves_messages_and_survives_session_reset() {
    let source = "## Heading\n\n**strong** and [Guide](https://e.test)\n\nunsafe\x1b[2J";
    let mut state = State::default();
    state.push_message(Role::Assistant, "Soyo", source);
    state.set_markdown_display(MarkdownDisplay::Source);
    let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let shown = (0..18)
        .map(|row| row_text(terminal.backend().buffer(), row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(shown.contains("## Heading"));
    assert!(shown.contains("**strong** and [Guide](https://e.test)"));
    assert!(shown.contains("\\u{1b}[2J"));
    assert_eq!(state.messages[0].text, source);
    assert!(state.footnote_hits.is_empty());
    state.set_markdown_display(MarkdownDisplay::Render);
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let shown = (0..18)
        .map(|row| row_text(terminal.backend().buffer(), row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(shown.contains("Heading"));
    assert!(!shown.contains("## Heading"));
    assert!(!shown.contains("**strong**"));
    assert_eq!(state.messages[0].text, source);
    state.set_markdown_display(MarkdownDisplay::Source);
    state.clear_messages();
    assert_eq!(state.markdown_display, MarkdownDisplay::Source);
    assert!(state.footnote_positions.is_empty() && state.footnote_history.is_empty());
}

#[test]
fn footnote_click_and_return_follow_semantic_targets_across_resize() {
    let source = format!(
        "REFERENCE_NOTE[^note].\n\n{}\n\n[^note]: TARGET_NOTE",
        "filler paragraph\n\n".repeat(30)
    );
    let mut state = State::default();
    state.push_message(Role::Assistant, "Soyo", &source);
    let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
    state.scroll_up(usize::MAX);
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let reference = state
        .footnote_hits
        .iter()
        .find(|hit| !hit.location.definition)
        .unwrap();
    let point = (reference.area.x, reference.area.y);
    assert!(state.footnote_at(point.0, point.1));
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let shown = (0..8)
        .map(|row| row_text(terminal.backend().buffer(), row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(shown.contains("TARGET_NOTE"));
    let definition = state
        .footnote_hits
        .iter()
        .find(|hit| hit.location.definition)
        .unwrap();
    let point = (definition.area.x, definition.area.y);
    assert!(state.footnote_at(point.0, point.1));
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let shown = (0..8)
        .map(|row| row_text(terminal.backend().buffer(), row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(shown.contains("REFERENCE_NOTE"));
    assert!(state.jump_footnote(1));
    terminal.resize(Rect::new(0, 0, 20, 8)).unwrap();
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let shown = (0..8)
        .map(|row| row_text(terminal.backend().buffer(), row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(shown.contains("TARGET_NOTE"));
    assert!(state.footnote_back());
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let shown = (0..8)
        .map(|row| row_text(terminal.backend().buffer(), row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(shown.contains("REFERENCE_NOTE"));
    assert_eq!(state.messages[0].text, source);
}

#[test]
fn footnote_numbers_are_scoped_to_messages_and_disabled_in_source_mode() {
    let mut state = State::default();
    state.push_message(Role::Assistant, "old", "Old[^same].\n\n[^same]: OLD_NOTE");
    let newest = state.push_message(Role::Assistant, "new", "New[^same].\n\n[^same]: NEW_NOTE");
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    assert!(state.jump_footnote(1));
    assert_eq!(state.pending_footnote.as_ref().unwrap().message, newest);
    assert!(!state.jump_footnote(0));
    assert!(!state.jump_footnote(999));
    let point = state
        .footnote_hits
        .first()
        .map(|hit| (hit.area.x, hit.area.y))
        .unwrap();
    state.command_palette = Some(0);
    assert!(!state.footnote_at(point.0, point.1));
    state.command_palette = None;
    state.set_markdown_display(MarkdownDisplay::Source);
    assert!(!state.footnote_at(point.0, point.1));
    assert!(!state.jump_footnote(1));
    assert!(!state.footnote_back());
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    assert!(state.footnote_positions.is_empty() && state.footnote_hits.is_empty());
    state.clear_messages();
    assert!(state.pending_footnote.is_none() && state.footnote_history.is_empty());
    assert_eq!(state.markdown_display, MarkdownDisplay::Source);
}

#[test]
fn literal_undefined_and_oversized_footnotes_do_not_create_hit_targets() {
    let mut state = State::default();
    state.push_message(
        Role::Assistant,
        "Soyo",
        "`[^literal]` and unresolved[^missing].",
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    assert!(state.footnote_positions.is_empty() && state.footnote_hits.is_empty());
    let key = "large".repeat(100);
    state.clear_messages();
    state.push_message(
        Role::Assistant,
        "Soyo",
        format!("Reference[^{key}].\n\n[^{key}]: Large identifier"),
    );
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    assert!(state.footnote_positions.is_empty() && state.footnote_hits.is_empty());
}

#[test]
fn markdown_table_reflows_unicode_and_link_styles_across_viewport_widths() {
    let source = "| Kind | Result |\n| :--- | ---: |\n| CJK | **加粗** |\n| Strong | __bold__ |\n| Unicode | `é` |\n| Link | [Guide](https://e.test) |";
    let mut state = State::default();
    state.push_message(Role::Assistant, "Soyo", source);
    for width in [80, 20, 40, 80] {
        let mut terminal = Terminal::new(TestBackend::new(width, 80)).unwrap();
        terminal
            .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let visible = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for token in ["CJK", "Strong", "Unicode", "Link", "加", "粗", "é"] {
            assert!(visible.contains(token), "{token} at width {width}");
        }
        let bold = buffer
            .content
            .iter()
            .filter(|cell| cell.modifier.contains(Modifier::BOLD))
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(bold.contains("bold"), "width {width}");
        let link = buffer
            .content
            .iter()
            .filter(|cell| cell.fg == BLUE && cell.modifier.contains(Modifier::UNDERLINED))
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(link.contains("Guide"), "width {width}");
        assert!(link.contains("https://e.test"), "width {width}");
        assert!(!visible.contains("**"));
        assert!(!visible.contains("__bold__"));
        assert!(!visible.contains(":---"));
        if width >= 40 {
            for row in 0..buffer.area.height {
                assert_eq!(buffer[(0, row)].symbol(), " ");
                assert_eq!(buffer[(width - 1, row)].symbol(), " ");
            }
        }
        assert_eq!(state.messages[0].text, source);
    }
}

#[test]
fn streaming_markdown_completes_tables_and_links_without_rewriting_history() {
    let mut state = State::default();
    let mut source = "| Name | Value |\n| --- |".to_owned();
    let id = state.push_message(Role::Assistant, "Soyo", &source);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    assert!(row_text(terminal.backend().buffer(), 1).contains("| Name | Value |"));
    let delta = " --- |\n| row | **ok** |\n\n[Guide](https://e.test";
    source.push_str(delta);
    assert!(state.append_message(id, delta));
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let visible = buffer
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(visible.contains('┌'));
    assert!(visible.contains("row"));
    assert!(!visible.contains("**ok**"));
    assert!(visible.contains("[Guide](https://e.test"));
    assert_eq!(state.messages[0].text, source);
    source.push(')');
    assert!(state.append_message(id, ")"));
    state.finish_message(id);
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let visible = buffer
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(visible.contains("Guide (https://e.test)"));
    assert!(!visible.contains("[Guide]("));
    assert_eq!(state.messages[0].text, source);
    assert!(!state.messages[0].streaming);
}

#[test]
fn markdown_table_rows_remain_reachable_through_transcript_scrolling() {
    let mut source = "| Name | Value |\n| --- | --- |\n".to_owned();
    for row in 0..40 {
        source.push_str(&format!("| r{row:02} | v{row:02} |\n"));
    }
    let mut state = State::default();
    state.push_message(Role::Assistant, "Soyo", &source);
    let mut terminal = Terminal::new(TestBackend::new(40, 6)).unwrap();
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let bottom = (0..6)
        .map(|row| row_text(terminal.backend().buffer(), row))
        .collect::<String>();
    assert!(bottom.contains("r39"));
    assert!(bottom.contains("v39"));
    assert!(state.last_line_count > 40);
    state.scroll_up(usize::MAX);
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let top = (0..6)
        .map(|row| row_text(terminal.backend().buffer(), row))
        .collect::<String>();
    assert!(top.contains("Name"));
    assert!(top.contains("r00"));
    assert!(!top.contains("r39"));
    state.scroll_down(usize::MAX);
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let bottom_again = (0..6)
        .map(|row| row_text(terminal.backend().buffer(), row))
        .collect::<String>();
    assert_eq!(bottom_again, bottom);
    assert_eq!(state.messages[0].text, source);
}

#[test]
fn markdown_links_and_emphasis_are_presentation_for_assistant_messages() {
    let source = "**bold** [Guide](https://e.test)";
    let mut state = State::default();
    for role in [Role::Assistant, Role::User, Role::Tool] {
        state.push_message(role, "", source);
    }
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    assert!(row_text(buffer, 1).contains("bold Guide (https://e.test)"));
    assert!(row_text(buffer, 4).contains(source));
    assert!(row_text(buffer, 7).contains(source));
    assert!(state.messages.iter().all(|message| message.text == source));
}

#[test]
fn status_icons_distinguish_attention_failure_and_progress_without_relayout() {
    let mut state = State::default();
    for (status, expected) in [
        ("Ready", GREEN),
        ("Completed", GREEN),
        ("Running", BLUE),
        ("Reviewing", BLUE),
        ("Awaiting approval", AMBER),
        ("Awaiting input", AMBER),
        ("NeedsConfirmation", AMBER),
        ("Failed", RED),
        ("Cancelled", MUTED),
    ] {
        state.status = status.into();
        let mut terminal = Terminal::new(TestBackend::new(60, 1)).unwrap();
        terminal
            .draw(|frame| draw_status(frame, &state, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].fg, expected, "{status}");
        assert_eq!(buffer[(2, 0)].fg, MUTED, "{status}");
        assert!(row_text(buffer, 0).ends_with("auto"));
    }
}

#[test]
fn activity_phases_use_the_active_mode_and_yield_to_status_and_permissions() {
    for mode in ["WORK", "CODE", "PLAN", "BUILD"] {
        let mut state = State {
            mode: mode.into(),
            status: "Running".into(),
            activity_active: true,
            ..State::default()
        };
        for (phase, expected) in [
            (0, "       █"),
            (1, "      █▓"),
            (2, "     █▓▒"),
            (3, "    █▓▒░"),
            (4, "   █▓▒░ "),
            (5, "  █▓▒░  "),
            (6, " █▓▒░   "),
            (7, "█▓▒░    "),
            (8, "▓█░     "),
            (9, "▒▓█     "),
            (10, "░▒▓█    "),
            (11, " ░▒▓█   "),
            (12, "  ░▒▓█  "),
            (13, "   ░▒▓█ "),
            (14, "    ░▒▓█"),
        ] {
            state.activity_frame = phase;
            let mut terminal = Terminal::new(TestBackend::new(40, 1)).unwrap();
            terminal
                .draw(|frame| draw_status(frame, &state, frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let animation = (10..18)
                .map(|column| buffer[(column, 0)].symbol())
                .collect::<String>();
            assert_eq!(animation, expected, "{mode}, phase {phase}");
            for column in 10..18 {
                let cell = &buffer[(column, 0)];
                assert_eq!(cell.fg, mode_color(mode));
                assert_eq!(
                    cell.modifier.contains(Modifier::DIM),
                    matches!(cell.symbol(), "▓" | "▒" | "░"),
                );
            }
            assert!(row_text(buffer, 0).ends_with("auto"));
        }
        for (permissions, expected) in [
            ("auto", "auto"),
            ("Manual Approval", "manual"),
            ("bypass permissions", "bypass"),
        ] {
            state.permissions = permissions.into();
            state.status = "Responding".into();
            state.activity_frame = 5;
            let mut terminal = Terminal::new(TestBackend::new(20, 1)).unwrap();
            terminal
                .draw(|frame| draw_status(frame, &state, frame.area()))
                .unwrap();
            let line = row_text(terminal.backend().buffer(), 0);
            assert!(line.starts_with("● Responding"), "{line}");
            assert!(line.ends_with(expected), "{line}");
            assert!(!line.contains(['█', '▓', '▒', '░']), "{line}");
        }
    }
}

#[test]
fn activity_stops_during_human_waits_and_terminal_states() {
    let mut state = State {
        activity_active: true,
        activity_frame: 4,
        ..State::default()
    };
    for status in [
        "Ready",
        "Running",
        "Thinking",
        "Responding",
        "Reviewing",
        "Cancelling",
    ] {
        state.status = status.into();
        assert!(state.activity_visible(), "{status}");
    }
    for status in [
        "Awaiting approval",
        "Awaiting input",
        "NeedsConfirmation",
        "NeedsInput",
        "NeedsAuth",
        "Completed",
        "Failed",
        "Cancelled",
        "LimitReached",
        "Preview",
        "Unknown",
    ] {
        state.status = status.into();
        assert!(!state.activity_visible(), "{status}");
        let mut terminal = Terminal::new(TestBackend::new(60, 1)).unwrap();
        terminal
            .draw(|frame| draw_status(frame, &state, frame.area()))
            .unwrap();
        assert!(!row_text(terminal.backend().buffer(), 0).contains(['█', '▓', '▒', '░']));
        assert_eq!(state.activity_frame, 4);
    }
    state.status = "Running".into();
    state.activity_active = false;
    assert!(!state.activity_visible());
    assert!(!State::default().activity_visible());
}

#[test]
fn run_summary_wraps_plain_metadata_and_colors_only_its_mode() {
    let metadata = "fixture-model · 1m 19s · 143.5 tok/s · 12.3k tokens";
    for mode in ["WORK", "CODE", "PLAN", "BUILD"] {
        for width in [20, 40, 80] {
            let mut state = State::default();
            let id = state.push_message(Role::RunSummary, mode, metadata);
            let mut terminal = Terminal::new(TestBackend::new(width, 10)).unwrap();
            terminal
                .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let start = u16::from(width >= 40);
            assert!(
                row_text(buffer, 0)
                    .trim_start()
                    .starts_with(&format!("{mode} · "))
            );
            for column in start..start + mode.len() as u16 {
                assert_eq!(buffer[(column, 0)].fg, mode_color(mode));
            }
            let actual = buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect::<String>();
            let expected = format!("{mode} · {metadata}")
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect::<String>();
            assert_eq!(actual, expected);
            for row in 0..10 {
                for column in 0..width {
                    if row == 0 && (start..start + mode.len() as u16).contains(&column) {
                        continue;
                    }
                    let cell = &buffer[(column, row)];
                    if cell.symbol() != " " {
                        assert_eq!(cell.fg, MUTED);
                        assert!(!cell.modifier.contains(Modifier::BOLD));
                    }
                }
            }
            assert!(state.message_hits.is_empty());
            assert!(!state.toggle_message(id, 0));
            assert_eq!(state.messages[0].text, metadata);
        }
    }
}

#[test]
fn run_summary_keeps_markdown_literal_and_escapes_control_metadata() {
    let mut state = State::default();
    let source = "**model** [ref](https://example.test) `code` [^1] 界\x1b[2J\u{85}\u{202e}";
    state.push_message(Role::RunSummary, "CODE", source);
    let mut terminal = Terminal::new(TestBackend::new(120, 6)).unwrap();
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let line = row_text(terminal.backend().buffer(), 0);
    assert!(line.contains("**model** [ref](https://example.test) `code` [^1] 界"));
    assert!(line.contains("\\u{1b}[2J"));
    assert!(line.contains("\\u{85}"));
    assert!(line.contains("\\u{202e}"));
    assert!(!line.contains("System"));
    assert!(!line.contains("Soyo"));
    assert!(state.footnote_hits.is_empty());
    assert!(state.message_hits.is_empty());
    assert_eq!(state.messages[0].text, source);
}

#[test]
fn run_summary_reports_metadata_truncation_at_the_retained_tail() {
    let mut state = State::default();
    state.push_message(Role::RunSummary, "CODE", "x".repeat(MAX_MESSAGE_BYTES + 1));
    assert!(state.messages[0].truncated);
    let mut terminal = Terminal::new(TestBackend::new(40, 6)).unwrap();
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let tail = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    assert!(tail.ends_with("·truncated"));
    assert!(state.messages[0].text.len() <= MAX_MESSAGE_BYTES);
}

#[test]
fn empty_draft_mode_hint_yields_to_palette_and_completion() {
    let mut state = State::default();
    for (width, hint) in [
        (80, "Tab mode · Ctrl+P commands"),
        (60, "Tab mode · Ctrl+P"),
        (30, "Ctrl+P"),
    ] {
        let mut terminal = Terminal::new(TestBackend::new(width, 18)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert!(row_text(terminal.backend().buffer(), 0).contains(hint));
    }
    state.set_details(Details {
        title: "Commands".into(),
        lines: vec!["palette".into()],
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    assert!(row_text(terminal.backend().buffer(), 0).contains("Tab mode"));
    state.command_palette = Some(0);
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    assert!(!row_text(terminal.backend().buffer(), 0).contains("Tab mode"));
    state.command_palette = None;
    state.details = None;
    state.editor.set_text("/per");
    state.completion.update("/per", 4, true);
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let top = row_text(terminal.backend().buffer(), 0);
    assert!(top.contains("↑↓ Tab/→ Esc"));
    assert!(!top.contains("Tab mode"));
}

#[test]
fn thinking_and_long_tools_fold_by_stable_id_and_keep_stream_updates() {
    let mut state = State::default();
    let id = state.push_message(
        Role::Thinking,
        "provider thought text",
        "private thought marker",
    );
    state.append_message(id, " · more");
    let image = render(&mut state, 80, 24);
    assert!(image.contains("▶ Thinking"));
    assert!(image.contains("▶ Thinking · running"));
    assert!(!image.contains("private thought marker"));
    let header = state.message_hits[0].area;
    assert!(state.toggle_at(header.x, header.y));
    assert!(render(&mut state, 80, 24).contains("private thought marker"));
    state.append_message(id, " · streamed addition");
    assert!(render(&mut state, 80, 24).contains("streamed addition"));
    assert!(state.expanded_messages.contains(&id));
    state.finish_message(id);
    assert!(render(&mut state, 80, 24).contains("Thinking · done"));
    let body = state.message_hits[0].area.bottom();
    assert!(!state.toggle_at(header.x, body));
    assert!(state.toggle_latest());
    assert!(!render(&mut state, 80, 24).contains("private thought marker"));

    for (text, expected) in [
        ("x".repeat(512), false),
        ("x".repeat(513), true),
        ("a\nb\nc\nd\ne\nf".into(), false),
        ("a\nb\nc\nd\ne\nf\ng".into(), true),
    ] {
        let tool_id = state.push_message(Role::Tool, "exec · call-1", &text);
        assert_eq!(foldable(state.messages.back().unwrap()), expected);
        assert_eq!(state.messages.back().unwrap().text, text);
        if expected {
            assert!(state.toggle_latest());
            assert!(state.expanded_messages.contains(&tool_id));
        }
    }
}

#[test]
fn header_hits_ignore_forged_body_popup_occlusion_and_stale_resizes() {
    let mut state = State::default();
    state.push_message(Role::User, "You", "▶ Thinking · forged\n▶ Tool · forged");
    let thought = state.push_message(Role::Thinking, "provider", "actual source");
    render(&mut state, 80, 24);
    let hit = state.message_hits[0].area;
    assert!(!state.toggle_at(hit.x, hit.y - 2));
    assert!(state.expanded_messages.is_empty());
    assert!(state.toggle_at(hit.x, hit.y));
    assert!(state.expanded_messages.contains(&thought));
    render(&mut state, 20, 6);
    assert!(state.message_hits.is_empty());
    assert!(!state.toggle_at(hit.x, hit.y));

    state.clear_messages();
    state.push_message(Role::Thinking, "provider", "new thought");
    state.editor.set_text("/per");
    state.completion.update("/per", 4, true);
    render(&mut state, 80, 12);
    let hit = state.message_hits[0].area;
    assert!(contains(state.completion_viewport, hit.x + 1, hit.y));
    assert!(!state.toggle_at(hit.x + 1, hit.y));
    assert!(!state.scroll_at(hit.x + 1, hit.y, true));
    assert!(state.expanded_messages.is_empty());
}

#[test]
fn fold_anchor_keeps_opened_header_visible_and_eviction_releases_fold_state() {
    let mut state = State::default();
    let id = state.push_message(Role::Tool, "exec · call-long", "line\n".repeat(200));
    state.push_message(Role::Assistant, "Soyo", "result");
    render(&mut state, 80, 24);
    let row = state.message_hits[0].area.y;
    assert!(state.toggle_at(3, row));
    render(&mut state, 80, 24);
    assert_eq!(state.message_hits[0].id, id);
    assert_eq!(state.message_hits[0].area.y, row);
    assert!(state.scroll > 0);
    state.scroll_to_bottom();
    render(&mut state, 80, 24);
    assert!(state.message_hits.is_empty());
    assert!(state.toggle_latest());
    render(&mut state, 80, 24);
    assert_eq!(state.message_hits[0].id, id);
    assert!(state.toggle_latest());
    assert!(state.expanded_messages.contains(&id));
    for _ in 0..MAX_MESSAGES {
        state.push_message(Role::User, "You", "next");
    }
    assert!(!state.expanded_messages.contains(&id));
    assert!(state.expanded_messages.len() <= MAX_MESSAGES);
    state.clear_messages();
    assert!(state.expanded_messages.is_empty());
    assert!(state.message_hits.is_empty());
    assert!(!state.toggle_latest());
}

#[test]
fn mouse_wheel_targets_only_actual_conversation_and_details_viewports() {
    let mut state = State::default();
    state.push_message(Role::Assistant, "Soyo", "line\n".repeat(200));
    state.set_details(Details {
        title: "Help".into(),
        lines: (0..100).map(|index| format!("line {index}")).collect(),
    });
    render(&mut state, 160, 45);
    let transcript = state.transcript_viewport.unwrap();
    let details = state.details_viewport.unwrap();
    assert!(state.scroll_at(transcript.x + 1, transcript.y + 1, true));
    assert_eq!(state.scroll, 3);
    assert_eq!(state.details_scroll, 0);
    assert!(state.scroll_at(transcript.x + 1, transcript.y + 1, false));
    assert_eq!(state.scroll, 0);
    assert!(state.scroll_at(details.x + 1, details.y + 1, false));
    assert_eq!(state.details_scroll, 3);
    assert!(state.scroll_at(details.x + 1, details.y + 1, true));
    assert_eq!(state.details_scroll, 0);
    for (x, y) in [(0, 0), (3, 39), (3, 41), (121, 4)] {
        assert!(!state.scroll_at(x, y, true));
    }
    render(&mut state, 20, 6);
    assert!(state.transcript_viewport.is_none());
    assert!(state.scroll_at(2, 2, false));
    assert_eq!(state.details_scroll, 3);
    assert!(!state.toggle_at(2, 2));
}

#[test]
fn hit_mapping_tracks_only_headers_retained_inside_the_render_line_budget() {
    let mut state = State::default();
    let source = "thought body\n".repeat(MAX_RENDER_LINES + 32);
    let id = state.push_message(Role::Thinking, "provider", &source);
    assert!(state.toggle_latest());
    state.scroll_to_bottom();
    render(&mut state, 80, 24);
    assert!(state.message_hits.is_empty());
    state.scroll_up(usize::MAX);
    assert!(render(&mut state, 80, 24).contains("earlier display lines omitted"));
    assert!(state.message_hits.is_empty());
    assert!(state.toggle_latest());
    render(&mut state, 80, 24);
    assert_eq!(state.message_hits.len(), 1);
    assert_eq!(state.message_hits[0].id, id);
    assert_eq!(state.messages[0].text, source);

    state.clear_messages();
    state.push_message(
        Role::Assistant,
        "Soyo",
        "▶ Thinking forged\n".repeat(MAX_RENDER_LINES + 32),
    );
    let actual = state.push_message(Role::Thinking, "provider", "hidden body");
    render(&mut state, 80, 24);
    let hit = state.message_hits[0].area;
    assert_eq!(state.message_hits.len(), 1);
    assert_eq!(state.message_hits[0].id, actual);
    assert!(!state.toggle_at(hit.x, hit.y - 2));
    assert!(state.toggle_at(hit.x, hit.y));
    assert!(state.expanded_messages.contains(&actual));
}

#[test]
fn folded_headers_reserve_visible_state_before_clipping_long_untrusted_labels() {
    let mut state = State::default();
    state.push_message(
        Role::Tool,
        "long-label-".repeat(30),
        "x".repeat(MAX_MESSAGE_BYTES + 1),
    );
    let image = render(&mut state, 80, 24);
    assert!(image.contains("▶ Tool · truncated"));
    assert_eq!(state.message_hits[0].area.height, 1);
    state.clear_messages();
    let id = state.push_message(Role::Thinking, "long-label-".repeat(30), "hidden");
    state.append_message(id, "");
    assert!(render(&mut state, 30, 10).contains("▶ Thinking · running"));
    assert_eq!(state.message_hits[0].area.height, 1);
}

#[test]
fn sidebar_tasks_keep_real_statuses_counts_and_bounded_omission_at_both_heights() {
    let mut state = State::default();
    assert!(render(&mut state, 120, 40).contains("No tasks"));
    state.todos = (0..64)
        .map(|index| TodoItem {
            id: format!("task-{index}"),
            content: if index == 1 {
                "unsafe\x1b[2J\u{202e} 中文👩‍💻".into()
            } else {
                format!("Task {index} {}", "content ".repeat(20))
            },
            status: match index {
                0 => TodoStatus::InProgress,
                1 => TodoStatus::Completed,
                _ => TodoStatus::Pending,
            },
        })
        .collect();
    for (height, tasks_row, extensions_row, first_task_row, omitted_row, omitted_count) in
        [(24, 12, 19, 13, 17, 60), (40, 20, 15, 21, 29, 56)]
    {
        let mut terminal = Terminal::new(TestBackend::new(120, height)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(row_text(buffer, tasks_row).contains("Tasks · 1/64"));
        assert!(row_text(buffer, extensions_row).contains("Extensions"));
        assert!(row_text(buffer, omitted_row).contains(&format!("+{omitted_count} more tasks")));
        assert_eq!(buffer[(91, first_task_row)].symbol(), "◐");
        assert_eq!(buffer[(91, first_task_row)].fg, BLUE);
        assert_eq!(buffer[(93, first_task_row)].fg, STRONG_FOREGROUND);
        assert!(
            buffer[(93, first_task_row)]
                .modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(buffer[(91, first_task_row + 1)].symbol(), "✓");
        assert_eq!(buffer[(91, first_task_row + 1)].fg, GREEN);
        assert_eq!(buffer[(93, first_task_row + 1)].fg, MUTED);
        let image = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(image.contains('…'));
        assert!(!image.contains('\x1b'));
        assert!(!image.contains('\u{202e}'));
    }
    state.clear_messages();
    assert!(state.todos.is_empty());
}

#[test]
fn canonical_short_tools_fold_with_real_summary_and_preserve_the_original_payload() {
    let source = serde_json::json!({
        "ok": true,
        "data": {"todos": [{"id": "one", "content": "UNIQUE_TASK_PAYLOAD", "status": "pending"}]},
        "error": null,
        "truncated": false,
    })
    .to_string();
    assert!(source.len() < 512);
    let mut state = State::default();
    let id = state.push_message(Role::Tool, "todo_write · call-summary · completed", &source);
    let mut terminal = Terminal::new(TestBackend::new(200, 8)).unwrap();
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    let header = row_text(terminal.backend().buffer(), 0);
    assert!(
        header.contains(
            "▶ Tool · todo_write · ✓ 1 task · 0 done/0 active · call-summary · completed"
        )
    );
    assert!(!render(&mut state, 110, 32).contains("UNIQUE_TASK_PAYLOAD"));
    assert!(state.toggle_latest());
    state.scroll_to_bottom();
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    assert!(row_text(terminal.backend().buffer(), 1).contains(&source));
    assert_eq!(state.messages[0].id, id);
    assert_eq!(
        state.messages[0].label,
        "todo_write · call-summary · completed"
    );
    assert_eq!(state.messages[0].text, source);
    assert!(state.toggle_at(3, 0));
    terminal
        .draw(|frame| draw_transcript(frame, &mut state, frame.area()))
        .unwrap();
    assert!(row_text(terminal.backend().buffer(), 0).starts_with(" ▶ Tool"));
    assert!(!row_text(terminal.backend().buffer(), 1).contains("UNIQUE_TASK_PAYLOAD"));
}

#[test]
fn tool_status_colors_use_canonical_result_and_keep_truncation_visible() {
    let mut state = State::default();
    state.push_message(
        Role::Tool,
        "read_file · call-error · completed",
        serde_json::json!({
            "ok": false, "data": null,
            "error": {"code": "io_error", "message": "missing file"}, "truncated": false,
        })
        .to_string(),
    );
    let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(
        row_text(buffer, 2).contains("Tool · read_file · ✕ io_error · missing file · call-error")
    );
    let marker = buffer
        .content
        .iter()
        .find(|cell| cell.symbol() == "✕")
        .unwrap();
    assert_eq!(marker.fg, RED);
    state.clear_messages();
    state.push_message(Role::Tool, "read_file · call-cut · completed", serde_json::json!({
            "ok": true, "error": null, "truncated": true,
            "data": {"content": "first\n", "start_line": 1, "end_line": 1, "version": "v1", "truncated": true},
        }).to_string());
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(
        row_text(buffer, 2)
            .contains("Tool · read_file · truncated · ✓ lines 1–1 · 6 bytes · call-cut")
    );
    assert!(
        buffer
            .content
            .iter()
            .any(|cell| cell.symbol() == "✓" && cell.fg == GREEN)
    );
    assert!(
        buffer
            .content
            .iter()
            .any(|cell| cell.symbol() == "t" && cell.fg == AMBER)
    );
}

#[test]
fn thinking_headers_are_compact_and_keep_availability_and_truncation_state() {
    let mut state = State::default();
    let id = state.push_message(
        Role::Thinking,
        "provider thought text",
        "retained thought body",
    );
    state.append_message(id, "");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(
        row_text(buffer, 2).trim_matches('│').trim(),
        "▶ Thinking · running"
    );
    assert_eq!(buffer[(2, 2)].fg, MUTED);
    assert_eq!(buffer[(2, 2)].bg, BACKGROUND);
    assert!(!row_text(buffer, 2).contains("provider"));
    assert!(!row_text(buffer, 2).contains("Ctrl+O"));
    state.finish_message(id);
    state.messages[0].truncated = true;
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    assert_eq!(
        row_text(terminal.backend().buffer(), 2)
            .trim_matches('│')
            .trim(),
        "▶ Thinking · done · truncated"
    );
    state.messages[0].label = "unavailable".into();
    state.messages[0].truncated = false;
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    assert_eq!(
        row_text(terminal.backend().buffer(), 2)
            .trim_matches('│')
            .trim(),
        "▶ Thinking · unavailable"
    );
    assert_eq!(state.messages[0].text, "retained thought body");
}

#[test]
fn input_hints_brand_permissions_and_sidebar_hierarchy_fit_the_existing_regions() {
    let mut state = State {
        permissions: "Manual Approval".into(),
        ..State::default()
    };
    state.editor.set_text("draft");
    for (width, hint) in [
        (80, "Enter send · Alt+Enter newline · Ctrl+O fold"),
        (69, "Enter send · Alt+Enter newline · Ctrl+O fold"),
        (68, "Enter send · Ctrl+O fold"),
        (67, "Enter send · Ctrl+O fold"),
        (43, "Enter send · Ctrl+O fold"),
        (42, ""),
        (41, ""),
        (20, ""),
    ] {
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        let title = row_text(buffer, 19);
        assert_eq!(buffer[(3, 19)].symbol(), "M");
        assert_eq!(buffer[(3, 20)].symbol(), "d");
        assert_eq!(buffer[(1, 19)].bg, INPUT_BACKGROUND);
        assert_eq!(buffer[(2, 19)].bg, INPUT_BACKGROUND);
        if hint.is_empty() {
            assert!(!title.contains("Enter send"));
        } else {
            assert!(title.contains(hint), "{width}: {title}");
        }
        assert_eq!(buffer[(0, 0)].fg, STRONG_FOREGROUND);
        assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
    }
    state.editor.truncated = true;
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(row_text(buffer, 35).contains("Message · paste truncated"));
    assert!(!row_text(buffer, 35).contains("Enter send"));
    for row in [2, 15, 20] {
        assert_eq!(buffer[(121, row)].fg, STRONG_FOREGROUND);
        assert!(buffer[(121, row)].modifier.contains(Modifier::BOLD));
    }
    for row in [1, 14, 19] {
        assert_eq!(buffer[(121, row)].symbol(), "─");
        assert_eq!(buffer[(121, row)].fg, BORDER);
    }
    let status = row_text(buffer, 34);
    let manual = status[..status.find("Manual Approval").unwrap()].width();
    assert_eq!(buffer[(manual as u16, 34)].fg, SOFT_AMBER);
    state.context_breakdown = vec![
        ContextEntry {
            label: "System".into(),
            tokens: 100,
        },
        ContextEntry {
            label: "Skills".into(),
            tokens: 0,
        },
    ];
    let rows = context_rows(&state, 38);
    assert_eq!(
        rows[0].spans.last().unwrap().style.fg,
        Some(STRONG_FOREGROUND)
    );
    assert_eq!(rows[1].spans.last().unwrap().style.fg, Some(MUTED));
}

#[test]
fn composer_placeholder_is_visual_and_excludes_user_text_questions_and_approvals() {
    let mut state = State::default();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let cursor = terminal.backend_mut().get_cursor_position().unwrap();
    assert_eq!((cursor.x, cursor.y), (3, 20));
    assert!(state.editor.text().is_empty());
    let buffer = terminal.backend().buffer();
    assert!(row_text(buffer, 20).contains("Describe a task…"));
    assert_eq!(buffer[(3, 20)].fg, MUTED);
    assert_eq!(buffer[(3, 20)].bg, INPUT_BACKGROUND);
    assert!(buffer[(3, 20)].modifier.contains(Modifier::ITALIC));
    assert_eq!(buffer[(3, 19)].fg, STRONG_FOREGROUND);
    assert!(buffer[(3, 19)].modifier.contains(Modifier::BOLD));

    state.editor.set_text("Describe a task…");
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(3, 20)].fg, FOREGROUND);
    assert!(!buffer[(3, 20)].modifier.contains(Modifier::ITALIC));
    assert_eq!(state.editor.text(), "Describe a task…");
    state.editor.clear();
    for status in [
        "Awaiting input",
        "NeedsInput",
        "Awaiting approval",
        "NeedsConfirmation",
    ] {
        state.status = status.into();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert!(
            !row_text(terminal.backend().buffer(), 20).contains("Describe a task…"),
            "{status}"
        );
        assert_eq!(terminal.backend().buffer()[(3, 20)].symbol(), " ");
    }
    state.status = "Ready".into();
    state.editor.truncated = true;
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(row_text(buffer, 19).contains("Message · paste truncated"));
    assert!(!row_text(buffer, 20).contains("Describe a task…"));
    assert_eq!(buffer[(3, 19)].fg, AMBER);
    assert!(buffer[(3, 19)].modifier.contains(Modifier::BOLD));
}

#[test]
fn composer_keycaps_and_padding_frame_multiline_unicode_and_completion_without_moving_cursor() {
    let mut state = State::default();
    state.editor.set_text("one\n界e\u{301}👩‍💻");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let cursor = terminal.backend_mut().get_cursor_position().unwrap();
    assert_eq!((cursor.x, cursor.y), (8, 20));
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(3, 19)].symbol(), "o");
    assert_eq!(buffer[(3, 20)].symbol(), "界");
    assert_eq!(buffer[(5, 20)].symbol(), "e\u{301}");
    assert_eq!(buffer[(6, 20)].symbol(), "👩‍💻");
    assert_eq!(buffer[(3, 22)].symbol(), "─");
    assert_eq!(buffer[(3, 22)].fg, INPUT_BORDER);
    assert_eq!(buffer[(3, 22)].bg, INPUT_BACKGROUND);

    state.editor.set_text("/per");
    state.completion.update("/per", 4, true);
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let cursor = terminal.backend_mut().get_cursor_position().unwrap();
    assert_eq!((cursor.x, cursor.y), (7, 20));
    assert_eq!(state.editor.text(), "/per");
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(7, 20)].symbol(), "m");
    assert_eq!(buffer[(7, 20)].fg, MUTED);
    assert!(!buffer[(7, 20)].modifier.contains(Modifier::ITALIC));
    assert!(!row_text(buffer, 20).contains("Describe a task…"));
    let title = row_text(buffer, 19);
    let enter = title[..title.find("Enter send").unwrap()].width() as u16;
    assert_eq!(buffer[(enter, 19)].fg, STRONG_FOREGROUND);
    assert_eq!(buffer[(enter, 19)].bg, CODE_BACKGROUND);
    assert!(buffer[(enter, 19)].modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(enter + 6, 19)].fg, MUTED);
    assert_eq!(buffer[(enter + 6, 19)].bg, INPUT_BACKGROUND);
    assert_eq!(buffer[(3, 22)].fg, INPUT_BORDER);
}

#[test]
fn tiny_composers_and_long_pastes_keep_real_input_cells_and_warning_priority() {
    let mut state = State::default();
    let mut terminal = Terminal::new(TestBackend::new(20, 6)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let cursor = terminal.backend_mut().get_cursor_position().unwrap();
    assert_eq!((cursor.x, cursor.y), (3, 4));
    assert_eq!(terminal.backend().buffer()[(3, 4)].symbol(), " ");
    assert!(!row_text(terminal.backend().buffer(), 3).contains("Enter"));
    assert!(!render(&mut state, 20, 6).contains("Describe a task…"));
    state.set_details(Details {
        title: "Help".into(),
        lines: vec!["Tool exec".into()],
    });
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let cursor = terminal.backend_mut().get_cursor_position().unwrap();
    assert_eq!((cursor.x, cursor.y), (3, 4));
    assert_eq!(terminal.backend().buffer()[(3, 4)].symbol(), " ");
    assert!(!row_text(terminal.backend().buffer(), 4).contains("Message"));
    assert!(!row_text(terminal.backend().buffer(), 4).contains('─'));

    state.details = None;
    state.editor.insert(&"界".repeat(30_000));
    assert!(state.editor.truncated);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let cursor = terminal.backend_mut().get_cursor_position().unwrap();
    assert!(cursor.x >= 3 && cursor.x < 78);
    assert_eq!(cursor.y, 20);
    let buffer = terminal.backend().buffer();
    assert!(row_text(buffer, 19).contains("Message · paste truncated at 64 KiB"));
    assert_eq!(buffer[(3, 19)].fg, AMBER);
    assert!(!row_text(buffer, 19).contains("Enter send"));
    assert!(!row_text(buffer, 20).contains("Describe a task…"));
    assert_eq!(buffer[(3, 22)].fg, INPUT_BORDER);
    assert!(
        state
            .editor
            .text()
            .is_char_boundary(state.editor.text().len())
    );
}

#[test]
fn active_mode_colors_link_the_third_cell_stripe_and_footer_at_all_sizes() {
    assert_eq!(UnicodeWidthStr::width(INPUT_STRIPE), 1);
    for (mode, color) in [
        ("WORK", Color::Rgb(106, 174, 252)),
        ("CODE", Color::Rgb(134, 207, 160)),
        ("PLAN", Color::Rgb(190, 163, 245)),
        ("BUILD", Color::Rgb(239, 178, 94)),
        ("FUTURE", MUTED),
    ] {
        for (width, height, input_top, input_height) in [
            (80, 24, 19, 4),
            (60, 18, 13, 4),
            (20, 6, 3, 2),
            (120, 40, 35, 4),
            (160, 45, 40, 4),
        ] {
            let mut state = State {
                mode: mode.into(),
                model: "fixture-model".into(),
                ..State::default()
            };
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let buffer = terminal.backend().buffer();
            let main_width = if width >= 120 {
                width - (width / 4).clamp(28, 40)
            } else {
                width
            };
            for row in input_top..input_top + input_height {
                assert_eq!(buffer[(1, row)].fg, color, "{mode}, {width}x{height}");
                assert_eq!(
                    buffer[(1, row)].bg,
                    INPUT_BACKGROUND,
                    "{mode}, {width}x{height}"
                );
                assert_eq!(buffer[(1, row)].symbol(), INPUT_STRIPE);
                assert_eq!(buffer[(main_width - 2, row)].symbol(), " ");
                assert_eq!(buffer[(main_width - 2, row)].bg, BACKGROUND);
                assert_eq!(buffer[(main_width - 3, row)].bg, INPUT_BACKGROUND);
            }
            for column in 1..=mode.len() as u16 {
                assert_eq!(
                    buffer[(column, height - 1)].fg,
                    color,
                    "{mode}, {width}x{height}"
                );
            }
            let model_column = mode.len() as u16 + 2;
            if buffer[(model_column, height - 1)].symbol() == "f" {
                assert_eq!(buffer[(model_column, height - 1)].fg, FOREGROUND);
            }
            assert_eq!(state.mode, mode);
            assert!(state.editor.text().is_empty());
            if width == 20 {
                state.set_details(Details {
                    title: "Help".into(),
                    lines: vec!["Review this operation".into()],
                });
                terminal.draw(|frame| draw(frame, &mut state)).unwrap();
                let buffer = terminal.backend().buffer();
                assert_eq!(buffer[(1, 4)].fg, color);
                assert_eq!(buffer[(1, 4)].bg, INPUT_BACKGROUND);
                assert_eq!(buffer[(1, 4)].symbol(), INPUT_STRIPE);
                assert_eq!(buffer[(1, 5)].fg, color);
            }
        }
    }
}

#[test]
fn composer_right_margin_keeps_short_and_scrolled_unicode_cursors_inside_the_surface() {
    for (width, height) in [(20, 6), (60, 18), (80, 24), (120, 40), (160, 45)] {
        let main_width = if width >= 120 {
            width - (width / 4).clamp(28, 40)
        } else {
            width
        };
        let gap = main_width - 2;
        let mut state = State::default();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        for with_details in [false, true] {
            if with_details {
                state.set_details(Details {
                    title: "Help".into(),
                    lines: vec!["Read the proposed operation".into()],
                });
            }
            for (draft, expected) in [("", 3), ("hello", 8), ("界", 5), ("/per", 7)] {
                state.editor.set_text(draft);
                state.completion.update(draft, state.editor.cursor(), true);
                terminal.draw(|frame| draw(frame, &mut state)).unwrap();
                let cursor = terminal.backend_mut().get_cursor_position().unwrap();
                assert_eq!(cursor.x, expected, "{width}×{height}, {draft:?}");
                assert!(cursor.x < gap);
                assert_eq!(terminal.backend().buffer()[(gap, cursor.y)].bg, BACKGROUND);
                if draft == "/per" {
                    assert_eq!(terminal.backend().buffer()[(7, cursor.y)].symbol(), "m");
                }
            }
            let draft = "界e\u{301}👩‍💻".repeat(40);
            state.editor.set_text(&draft);
            state.completion.update(&draft, state.editor.cursor(), true);
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let cursor = terminal.backend_mut().get_cursor_position().unwrap();
            assert_eq!(cursor.x, gap - 1, "{width}×{height}");
            assert_eq!(terminal.backend().buffer()[(gap, cursor.y)].bg, BACKGROUND);
            assert_eq!(state.editor.text(), draft);
        }
    }
}

#[test]
fn pending_mode_has_its_own_label_color_until_the_active_accent_changes_at_the_boundary() {
    for (current, next) in [
        ("WORK", "CODE"),
        ("CODE", "PLAN"),
        ("PLAN", "BUILD"),
        ("BUILD", "WORK"),
    ] {
        let mut state = State {
            mode: current.into(),
            pending_mode: Some(next.into()),
            ..State::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(row_text(buffer, 23).contains(&format!("{current}→{next}")));
        assert_eq!(buffer[(1, 19)].fg, mode_color(current));
        assert_eq!(buffer[(1, 19)].bg, INPUT_BACKGROUND);
        assert_eq!(buffer[(1, 23)].fg, mode_color(current));
        let arrow = 1 + current.len() as u16;
        assert_eq!(buffer[(arrow, 23)].fg, MUTED);
        for column in arrow + 1..arrow + 1 + next.len() as u16 {
            assert_eq!(buffer[(column, 23)].fg, mode_color(next));
        }
        let mut tiny = Terminal::new(TestBackend::new(20, 6)).unwrap();
        tiny.draw(|frame| draw(frame, &mut state)).unwrap();
        assert_eq!(tiny.backend().buffer()[(1, 3)].fg, mode_color(current));
        assert_eq!(tiny.backend().buffer()[(1, 3)].bg, INPUT_BACKGROUND);
        assert_eq!(tiny.backend().buffer()[(1, 5)].fg, mode_color(current));

        state.mode = next.into();
        state.pending_mode = None;
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(1, 19)].fg, mode_color(next));
        assert_eq!(buffer[(1, 19)].bg, INPUT_BACKGROUND);
        assert_eq!(buffer[(1, 23)].fg, mode_color(next));
        assert!(!row_text(buffer, 23).contains('→'));
    }
}

#[test]
fn command_modal_centers_catalog_rows_and_restores_the_real_editor_cursor() {
    let mut state = State::default();
    state.editor.set_text("draft界");
    state.set_details(Details {
        title: "Saved records".into(),
        lines: (0..80).map(|index| format!("record {index}")).collect(),
    });
    state.details_scroll = 3;
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    assert!(terminal.backend().cursor_visible());
    assert_eq!(terminal.backend().cursor_position(), (10, 20).into());
    state.command_palette = Some(0);
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    assert!(!terminal.backend().cursor_visible());
    let buffer = terminal.backend().buffer();
    for (x, y, symbol) in [(1, 5, "╭"), (78, 5, "╮"), (1, 18, "╰"), (78, 18, "╯")] {
        assert_eq!(buffer[(x, y)].symbol(), symbol);
    }
    assert!(row_text(buffer, 5).contains("Commands"));
    assert!(row_text(buffer, 5).contains(&format!("1/{}", COMMANDS.len())));
    assert!(row_text(buffer, 6).contains(COMMANDS[0].text));
    assert!(row_text(buffer, 6).contains(COMMANDS[0].description));
    assert_eq!(buffer[(2, 6)].symbol(), "›");
    assert_eq!(buffer[(2, 6)].bg, BLUE);
    assert!(buffer[(2, 6)].modifier.contains(Modifier::BOLD));
    assert_eq!(state.palette_hits.len(), 11);
    assert_eq!(state.palette_index_at(2, 6), Some(0));
    for (x, y) in [(2, 5), (1, 6), (2, 17), (0, 0)] {
        assert_eq!(state.palette_index_at(x, y), None);
    }
    assert_eq!(state.details.as_ref().unwrap().title, "Saved records");
    assert_eq!(state.details_scroll, 3);
    state.command_palette = None;
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    assert!(terminal.backend().cursor_visible());
    assert_eq!(terminal.backend().cursor_position(), (10, 20).into());
    assert_eq!(state.editor.text(), "draft界");
    assert_eq!(state.details_scroll, 3);
    assert!(state.palette_hits.is_empty());
    assert!(row_text(terminal.backend().buffer(), 2).contains("Saved records"));
}

#[test]
fn command_modal_clamps_selection_and_scrolls_through_the_shared_catalog() {
    let mut state = State {
        command_palette: Some(usize::MAX),
        ..State::default()
    };
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let last = COMMANDS.len() - 1;
    assert_eq!(state.command_palette, Some(last));
    assert_eq!(terminal.backend().buffer()[(36, 13)].symbol(), "╭");
    assert_eq!(terminal.backend().buffer()[(123, 26)].symbol(), "╯");
    assert_eq!(state.palette_hits.len(), 11);
    for (row, (area, index)) in state.palette_hits.iter().enumerate() {
        assert_eq!(*index, COMMANDS.len() - 11 + row);
        assert!(
            row_text(terminal.backend().buffer(), area.y)
                .contains(COMMANDS[*index].text.trim_end())
        );
        assert_eq!(state.palette_index_at(area.x + 3, area.y), Some(*index));
    }
    assert_eq!(terminal.backend().buffer()[(37, 24)].symbol(), "›");
    assert!(state.palette_scroll(false));
    assert_eq!(state.command_palette, Some(last));
    assert!(state.palette_scroll(true));
    assert_eq!(state.command_palette, Some(last - 1));
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    assert_eq!(state.palette_hits.last().unwrap().1, last);
    assert_eq!(state.palette_first, COMMANDS.len() - 11);
    assert_eq!(terminal.backend().buffer()[(37, 23)].symbol(), "›");
    let middle = state.palette_hits[3];
    state.command_palette = state.palette_index_at(middle.0.x + 3, middle.0.y);
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    assert_eq!(state.palette_first, COMMANDS.len() - 11);
    assert_eq!(
        terminal.backend().buffer()[(middle.0.x, middle.0.y)].symbol(),
        "›"
    );
    assert_eq!(
        state.palette_index_at(middle.0.x, middle.0.y),
        Some(middle.1)
    );
    state.command_palette = Some(0);
    assert!(state.palette_scroll(true));
    assert_eq!(state.command_palette, Some(0));
    assert!(state.palette_scroll(false));
    assert_eq!(state.command_palette, Some(1));
    state.command_palette = None;
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    assert_eq!(state.palette_first, 0);
    assert!(!state.palette_scroll(false));
    assert_eq!(state.palette_index_at(37, 24), None);
}

#[test]
fn command_modal_tiny_layout_keeps_selection_close_hint_and_fresh_resize_hits() {
    let mut state = State {
        command_palette: Some(COMMANDS.len() - 1),
        ..State::default()
    };
    state.editor.set_text("retained draft");
    let mut terminal = Terminal::new(TestBackend::new(20, 6)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(1, 0)].symbol(), "╭");
    assert_eq!(buffer[(18, 5)].symbol(), "╯");
    assert!(row_text(buffer, 0).contains("Commands"));
    assert!(row_text(buffer, 4).contains("↑↓ · Enter"));
    assert!(row_text(buffer, 5).contains("Esc close"));
    assert_eq!(buffer[(2, 3)].symbol(), "›");
    assert_eq!(state.palette_index_at(3, 3), Some(COMMANDS.len() - 1));
    assert_eq!(state.palette_hits.len(), 3);
    for row in 0..6 {
        assert_eq!(buffer[(0, row)].symbol(), " ");
        assert_eq!(buffer[(19, row)].symbol(), " ");
        assert_eq!(buffer[(0, row)].bg, BACKGROUND);
        assert_eq!(buffer[(19, row)].bg, BACKGROUND);
    }
    assert!(!terminal.backend().cursor_visible());
    terminal.backend_mut().resize(110, 32);
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    assert_eq!(state.palette_index_at(3, 3), None);
    assert_eq!(terminal.backend().buffer()[(11, 9)].symbol(), "╭");
    assert_eq!(state.palette_hits.len(), 11);
    assert_eq!(state.editor.text(), "retained draft");
    state.command_palette = Some(usize::MAX);
    render(&mut state, 1, 1);
    assert_eq!(state.command_palette, Some(COMMANDS.len() - 1));
    assert!(state.palette_hits.is_empty());
}

#[test]
fn command_modal_blocks_backdrop_actions_and_suppresses_completion_overlay() {
    let mut state = State::default();
    let id = state.push_message(Role::Thinking, "provider", "thought\n".repeat(100));
    state.set_details(Details {
        title: "Approval required".into(),
        lines: (0..100).map(|index| format!("argument {index}")).collect(),
    });
    state.details_scroll = 4;
    state.editor.set_text("/per");
    state.completion.update("/per", 4, true);
    render(&mut state, 160, 45);
    state.command_palette = Some(0);
    let image = render(&mut state, 160, 45);
    let scroll = state.scroll;
    let details_scroll = state.details_scroll;
    assert!(!image.contains("Command suggestions"));
    assert!(state.completion.visible());
    assert!(state.completion_viewport.is_none());
    assert!(!state.toggle_latest());
    assert!(!state.toggle_at(2, 2));
    assert!(!state.scroll_at(3, 3, true));
    assert!(!state.scroll_at(82, 3, false));
    assert!(!state.expanded_messages.contains(&id));
    assert_eq!(state.scroll, scroll);
    assert_eq!(state.details_scroll, details_scroll);
    assert_eq!(state.editor.text(), "/per");
    assert_eq!(state.details.as_ref().unwrap().title, "Approval required");
    state.command_palette = None;
    assert!(render(&mut state, 160, 45).contains("Command suggestions"));
}
#[test]
fn mcp_sidebar_uses_real_states_and_bounds_the_server_list_at_both_heights() {
    let mut state = State::default();
    assert!(render(&mut state, 160, 40).contains("MCP  No servers"));
    state.mcp_servers = (0..16)
        .map(|index| McpServerStatus {
            server: format!("fixture-{index}"),
            transport: McpTransport::Stdio,
            state: McpServerState::Configured,
            tools: 0,
        })
        .collect();
    let configured = render(&mut state, 160, 40);
    assert!(configured.contains("Configured"));
    assert!(!configured.contains("Connected"));
    assert!(configured.contains("+11 more"));
    state.mcp_servers[0].state = McpServerState::Connected;
    state.mcp_servers[0].tools = 3;
    let connected = render(&mut state, 160, 40);
    assert!(connected.contains("Connected"));
    assert!(connected.contains("stdio · 3t"));
    assert!(!connected.contains("fixture-5"));
    let short = render(&mut state, 120, 24);
    assert!(short.contains("MCP Connected +15 more"));
    state.mcp_servers[0].state = McpServerState::Stopped;
    assert!(render(&mut state, 160, 40).contains("Stopped"));
    for status in [
        McpServerState::Connecting,
        McpServerState::NeedsAuth,
        McpServerState::Failed,
    ] {
        state.mcp_servers[0].state = status;
        assert!(render(&mut state, 160, 40).contains(mcp_state(status).1));
    }
}
