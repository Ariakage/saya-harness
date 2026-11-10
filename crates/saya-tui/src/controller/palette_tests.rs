//! Modal selection is independent of drafts, details, and human authorization.
use super::control_tests::{approval, fixture};
use super::*;
use crate::view::Details;
use saya_core::{contract::PermissionMode, input::InputRequest};
use tokio::sync::oneshot;

fn toggle() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)
}
fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn render(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| view::draw(frame, &mut app.state))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}
fn command_point(app: &App, index: usize, width: u16, height: u16) -> (u16, u16) {
    for row in 0..height {
        for column in 0..width {
            if app.state.palette_index_at(column, row) == Some(index) {
                return (column, row);
            }
        }
    }
    panic!("command row {index} was not visible");
}
fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[tokio::test]
async fn help_and_profiles_remain_visible_over_a_pending_approval() {
    for (command, title) in [("/help", "Help"), ("/profiles", "Configured profiles · UI")] {
        let (_root, mut app) = fixture();
        let (request, mut receiver) = approval(&app, "pending-details");
        app.request(request);
        app.state.editor.set_text("y");
        app.key(toggle()).await.unwrap();
        app.state.command_palette = Some(
            COMMANDS
                .iter()
                .position(|suggestion| suggestion.text == command)
                .unwrap(),
        );
        app.key(press(KeyCode::Enter)).await.unwrap();
        assert_eq!(app.state.details.as_ref().unwrap().title, title);
        assert_eq!(app.state.editor.text(), "y");
        assert!(matches!(
            receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        app.key(press(KeyCode::Enter)).await.unwrap();
        assert_eq!(
            app.state.details.as_ref().unwrap().title,
            "Approval required"
        );
        assert_eq!(app.state.editor.text(), "y");
        assert!(matches!(
            receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        app.key(press(KeyCode::Enter)).await.unwrap();
        assert_eq!(receiver.await.unwrap().unwrap(), Some(true));
    }
}

#[tokio::test]
async fn toggling_the_modal_preserves_details_draft_and_scroll_and_captures_other_keys() {
    let (_root, mut app) = fixture();
    app.state.editor.set_text("草稿 🌸");
    app.state.set_details(Details {
        title: "Tools · UI snapshot".into(),
        lines: (0..100).map(|index| format!("record {index}")).collect(),
    });
    app.state.details_scroll = 7;
    let lines = app.state.details.as_ref().unwrap().lines.clone();
    app.key(toggle()).await.unwrap();
    assert_eq!(app.state.command_palette, Some(0));
    for code in [
        KeyCode::Char('y'),
        KeyCode::Char('n'),
        KeyCode::Tab,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Home,
        KeyCode::End,
    ] {
        app.key(press(code)).await.unwrap();
    }
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT))
        .await
        .unwrap();
    let mut repeated = toggle();
    repeated.kind = KeyEventKind::Repeat;
    app.key(repeated).await.unwrap();
    assert_eq!(app.state.command_palette, Some(0));
    assert_eq!(app.state.editor.text(), "草稿 🌸");
    assert_eq!(app.state.details.as_ref().unwrap().lines, lines);
    assert_eq!(app.state.details_scroll, 7);
    assert!(app.active.is_none());
    assert!(app.prepared.is_none());
    app.key(press(KeyCode::Esc)).await.unwrap();
    assert!(app.state.command_palette.is_none());
    assert_eq!(
        app.state.details.as_ref().unwrap().title,
        "Tools · UI snapshot"
    );
    assert_eq!(app.state.details_scroll, 7);
    let mut repeated_escape = press(KeyCode::Esc);
    repeated_escape.kind = KeyEventKind::Repeat;
    app.key(repeated_escape).await.unwrap();
    assert_eq!(
        app.state.details.as_ref().unwrap().title,
        "Tools · UI snapshot"
    );
    assert_eq!(app.state.details_scroll, 7);
    app.key(toggle()).await.unwrap();
    app.key(toggle()).await.unwrap();
    assert!(app.state.command_palette.is_none());
    assert_eq!(app.state.editor.text(), "草稿 🌸");
    assert_eq!(app.state.details_scroll, 7);
}

#[tokio::test]
async fn enter_executes_the_highlighted_command_and_repeated_enter_cannot_submit_the_hidden_draft()
{
    let (_root, mut app) = fixture();
    app.state.editor.set_text("/permissions bypass");
    app.key(toggle()).await.unwrap();
    app.key(press(KeyCode::Enter)).await.unwrap();
    assert_eq!(app.config.mode, WorkMode::Plan);
    assert_eq!(app.config.permissions_mode, PermissionMode::Auto);
    assert_eq!(app.state.editor.text(), "/permissions bypass");
    assert!(app.state.command_palette.is_none());
    let mut repeated = press(KeyCode::Enter);
    repeated.kind = KeyEventKind::Repeat;
    app.key(repeated).await.unwrap();
    assert_eq!(app.config.permissions_mode, PermissionMode::Auto);
    assert!(app.active.is_none());
    assert!(app.prepared.is_none());
    app.key(press(KeyCode::Enter)).await.unwrap();
    assert_eq!(app.config.permissions_mode, PermissionMode::Bypass);
    assert!(app.state.editor.text().is_empty());
}

#[tokio::test]
async fn argument_scaffolds_preserve_existing_slash_drafts_and_extreme_selection_is_bounded() {
    let (_root, mut app) = fixture();
    let scaffold = COMMANDS
        .iter()
        .rposition(|command| command.text == "/export-full markdown ")
        .unwrap();
    app.state.editor.set_text("/mode build");
    app.state.command_palette = Some(scaffold);
    app.key(press(KeyCode::Enter)).await.unwrap();
    assert_eq!(app.state.editor.text(), "/mode build");
    assert_eq!(app.config.mode, WorkMode::Work);
    assert!(app.ui_job.is_none());
    app.state.editor.clear();
    app.state.command_palette = Some(scaffold);
    app.key(press(KeyCode::Enter)).await.unwrap();
    assert_eq!(app.state.editor.text(), "/export-full markdown ");
    assert!(app.state.command_palette.is_none());
    assert!(app.ui_job.is_none());
    app.state.command_palette = Some(usize::MAX);
    app.key(press(KeyCode::Down)).await.unwrap();
    assert_eq!(app.state.command_palette, Some(COMMANDS.len() - 1));
}

#[tokio::test]
async fn modal_mouse_selects_without_execution_and_blocks_background_cards_and_scroll() {
    let (_root, mut app) = fixture();
    app.state
        .push_message(Role::Thinking, "provider", "thought stays folded");
    app.state.set_details(Details {
        title: "Tools · UI snapshot".into(),
        lines: (0..100).map(|index| format!("record {index}")).collect(),
    });
    app.state.details_scroll = 5;
    app.key(toggle()).await.unwrap();
    render(&mut app, 160, 45);
    let original_scroll = app.state.scroll;
    let original_details_scroll = app.state.details_scroll;
    let (x, y) = command_point(&app, 0, 160, 45);
    assert!(app.mouse(mouse(MouseEventKind::ScrollDown, x, y)));
    assert_eq!(app.state.command_palette, Some(1));
    render(&mut app, 160, 45);
    let (x, y) = command_point(&app, 2, 160, 45);
    assert!(app.mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, y)));
    assert_eq!(app.state.command_palette, Some(2));
    assert_eq!(app.config.mode, WorkMode::Work);
    assert!(!app.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 3, 2)));
    assert!(!app.mouse(mouse(MouseEventKind::ScrollDown, 0, 0)));
    assert_eq!(app.state.command_palette, Some(2));
    assert_eq!(app.state.scroll, original_scroll);
    assert_eq!(app.state.details_scroll, original_details_scroll);
    app.key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL))
        .await
        .unwrap();
    assert!(!app.state.toggle_latest());
    app.key(press(KeyCode::Enter)).await.unwrap();
    assert_eq!(app.config.mode, WorkMode::Code);
    assert!(app.state.command_palette.is_none());
    assert!(!render(&mut app, 160, 45).contains("thought stays folded"));
}

#[tokio::test]
async fn a_modal_above_approval_does_not_reply_and_closing_restores_the_latest_prompt() {
    let (_root, mut app) = fixture();
    let (request, mut receiver) = approval(&app, "modal-approval");
    app.request(request);
    app.state.details_scroll = 2;
    app.key(toggle()).await.unwrap();
    app.key(press(KeyCode::Char('y'))).await.unwrap();
    app.key(press(KeyCode::Char('n'))).await.unwrap();
    assert!(matches!(
        receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(app.state.editor.text().is_empty());
    app.key(press(KeyCode::Esc)).await.unwrap();
    assert!(app.state.command_palette.is_none());
    assert_eq!(
        app.state.details.as_ref().unwrap().title,
        "Approval required"
    );
    assert_eq!(app.state.details_scroll, 2);
    assert!(matches!(
        receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    app.key(press(KeyCode::Char('n'))).await.unwrap();
    assert_eq!(receiver.await.unwrap().unwrap(), Some(false));

    app.key(toggle()).await.unwrap();
    let (arriving, arriving_receiver) = approval(&app, "arriving");
    app.request(arriving);
    assert!(app.state.command_palette.is_some());
    assert_eq!(
        app.state.details.as_ref().unwrap().title,
        "Approval required"
    );
    drop(arriving_receiver);
    assert!(app.refresh_mode().unwrap());
    app.key(press(KeyCode::Esc)).await.unwrap();
    assert!(app.pending.is_none());
    assert!(app.state.details.is_none());
}

#[tokio::test]
async fn closing_a_modal_above_clarification_does_not_submit_a_repeated_enter() {
    let (_root, mut app) = fixture();
    let (reply, mut receiver) = oneshot::channel();
    app.request(Request::Input(
        InputRequest {
            question: "Choose a location".into(),
            options: vec![],
        },
        reply,
    ));
    app.state.editor.set_text("草稿回复");
    app.key(toggle()).await.unwrap();
    app.key(press(KeyCode::Esc)).await.unwrap();
    let mut repeated = press(KeyCode::Enter);
    repeated.kind = KeyEventKind::Repeat;
    app.key(repeated).await.unwrap();
    assert!(matches!(
        receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert_eq!(app.state.editor.text(), "草稿回复");
    app.key(press(KeyCode::Enter)).await.unwrap();
    assert_eq!(
        receiver.await.unwrap().unwrap().as_deref(),
        Some("草稿回复")
    );
}

#[tokio::test]
async fn an_async_details_replacement_cannot_submit_a_question_hidden_beneath_the_modal() {
    let (_root, mut app) = fixture();
    let (reply, mut receiver) = oneshot::channel();
    app.request(Request::Input(
        InputRequest {
            question: "Choose a location".into(),
            options: vec![],
        },
        reply,
    ));
    app.state.editor.set_text("old draft");
    app.key(toggle()).await.unwrap();
    app.state.set_details(Details {
        title: "Tools · UI snapshot".into(),
        lines: vec!["A previously started query completed".into()],
    });
    app.key(press(KeyCode::Esc)).await.unwrap();
    assert_eq!(
        app.state.details.as_ref().unwrap().title,
        "Tools · UI snapshot"
    );
    app.key(press(KeyCode::Enter)).await.unwrap();
    assert_eq!(app.state.details.as_ref().unwrap().title, "Question");
    assert_eq!(app.state.editor.text(), "old draft");
    assert!(matches!(
        receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    let mut repeated = press(KeyCode::Enter);
    repeated.kind = KeyEventKind::Repeat;
    app.key(repeated).await.unwrap();
    assert!(matches!(
        receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    app.key(press(KeyCode::Enter)).await.unwrap();
    assert_eq!(
        receiver.await.unwrap().unwrap().as_deref(),
        Some("old draft")
    );
}
