//! Shared palette for terminal surfaces and rendered message content.
use ratatui::style::Color;

pub(crate) const BACKGROUND: Color = Color::Rgb(9, 9, 11);
pub(crate) const SIDEBAR_BACKGROUND: Color = Color::Rgb(20, 20, 22);
pub(crate) const INPUT_BACKGROUND: Color = Color::Rgb(32, 32, 36);
pub(crate) const INPUT_BORDER: Color = Color::Rgb(42, 42, 48);
pub(crate) const BORDER: Color = Color::Rgb(48, 48, 56);
pub(crate) const FOREGROUND: Color = Color::Rgb(212, 212, 216);
pub(crate) const STRONG_FOREGROUND: Color = Color::Rgb(232, 232, 237);
pub(crate) const MUTED: Color = Color::Rgb(129, 129, 139);
pub(crate) const BLUE: Color = Color::Rgb(88, 166, 255);
pub(crate) const HEADING: Color = Color::Rgb(187, 154, 247);
pub(crate) const INLINE_CODE: Color = Color::Rgb(125, 203, 155);
pub(crate) const AMBER: Color = Color::Rgb(246, 173, 85);
pub(crate) const SOFT_AMBER: Color = Color::Rgb(187, 166, 133);
pub(crate) const CODE_BACKGROUND: Color = Color::Rgb(23, 23, 27);
pub(crate) const CODE_FOREGROUND: Color = Color::Rgb(196, 196, 204);
pub(crate) const GREEN: Color = Color::Rgb(125, 203, 155);
pub(crate) const RED: Color = Color::Rgb(248, 113, 113);

/// Accent shared by the active mode label and the message input stripe.
pub(crate) fn mode_color(mode: &str) -> Color {
    match mode {
        "WORK" => BLUE,
        "CODE" => GREEN,
        "PLAN" => HEADING,
        "BUILD" => AMBER,
        _ => MUTED,
    }
}
