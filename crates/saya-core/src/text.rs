//! Character classification shared by terminal-facing text projections.

/// Identify Unicode control characters and direction-formatting controls.
/// Newline and tab are included. Callers choose their own layout exceptions
/// and whether to replace, escape, or remove a matching character.
pub fn is_terminal_control(character: char) -> bool {
    character.is_control()
        || matches!(character,
            '\u{061c}' | '\u{200e}' | '\u{200f}'
            | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{206f}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_all_c0_c1_and_direction_control_ranges() {
        for value in (0..=0x1f)
            .chain(0x7f..=0x9f)
            .chain([0x061c, 0x200e, 0x200f])
            .chain(0x202a..=0x202e)
            .chain(0x2066..=0x206f)
        {
            assert!(
                is_terminal_control(char::from_u32(value).unwrap()),
                "U+{value:04X}"
            );
        }
    }

    #[test]
    fn ordinary_unicode_and_grapheme_components_remain_text() {
        for character in
            "Saya 中文 日本語 한글 العربية e\u{301} 👩\u{200d}💻 🌸 <tag> & \\ /\"".chars()
        {
            assert!(
                !is_terminal_control(character),
                "U+{:04X}",
                character as u32
            );
        }
    }
}
