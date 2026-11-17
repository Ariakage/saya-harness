//! Static slash-command discovery and completion without runtime side effects.
use crate::i18n::command as text;

/// A command or argument scaffold the user can insert into the editor.
#[derive(Debug, Clone, Copy)]
pub struct Suggestion {
    pub text: &'static str,
    pub description: &'static str,
}

/// The first 16 entries retain the existing command-palette order.
pub const COMMANDS: &[Suggestion] = &[
    Suggestion {
        text: "/mode plan",
        description: text::MODE_PLAN,
    },
    Suggestion {
        text: "/mode build",
        description: text::MODE_BUILD,
    },
    Suggestion {
        text: "/mode code",
        description: text::MODE_CODE,
    },
    Suggestion {
        text: "/mode work",
        description: text::MODE_WORK,
    },
    Suggestion {
        text: "/permissions auto",
        description: text::PERMISSIONS_AUTO,
    },
    Suggestion {
        text: "/permissions manual",
        description: text::PERMISSIONS_MANUAL,
    },
    Suggestion {
        text: "/permissions bypass",
        description: text::PERMISSIONS_BYPASS,
    },
    Suggestion {
        text: "/sessions",
        description: text::SESSIONS,
    },
    Suggestion {
        text: "/new",
        description: text::NEW,
    },
    Suggestion {
        text: "/profiles",
        description: text::PROFILES,
    },
    Suggestion {
        text: "/tools",
        description: text::TOOLS,
    },
    Suggestion {
        text: "/jobs",
        description: text::JOBS,
    },
    Suggestion {
        text: "/status",
        description: text::STATUS,
    },
    Suggestion {
        text: "/cancel",
        description: text::CANCEL,
    },
    Suggestion {
        text: "/help",
        description: text::HELP,
    },
    Suggestion {
        text: "/exit",
        description: text::EXIT,
    },
    Suggestion {
        text: "/ps",
        description: text::PS,
    },
    Suggestion {
        text: "/compact",
        description: text::COMPACT,
    },
    Suggestion {
        text: "/memory",
        description: text::MEMORY,
    },
    Suggestion {
        text: "/profile ",
        description: text::PROFILE,
    },
    Suggestion {
        text: "/resume ",
        description: text::RESUME,
    },
    Suggestion {
        text: "/resume-accept ",
        description: text::RESUME_ACCEPT,
    },
    Suggestion {
        text: "/cancel-job ",
        description: text::CANCEL_JOB,
    },
    Suggestion {
        text: "/markdown render",
        description: text::MARKDOWN_RENDER,
    },
    Suggestion {
        text: "/markdown source",
        description: text::MARKDOWN_SOURCE,
    },
    Suggestion {
        text: "/footnote ",
        description: text::FOOTNOTE,
    },
    Suggestion {
        text: "/footnote back",
        description: text::FOOTNOTE_BACK,
    },
    Suggestion {
        text: "/export json ",
        description: text::EXPORT_JSON,
    },
    Suggestion {
        text: "/export markdown ",
        description: text::EXPORT_MARKDOWN,
    },
    Suggestion {
        text: "/export full json ",
        description: text::EXPORT_FULL_JSON,
    },
    Suggestion {
        text: "/export full markdown ",
        description: text::EXPORT_FULL_MARKDOWN,
    },
    Suggestion {
        text: "/export-full json ",
        description: text::EXPORT_FULL_JSON,
    },
    Suggestion {
        text: "/export-full markdown ",
        description: text::EXPORT_FULL_MARKDOWN,
    },
    Suggestion {
        text: "/cd ",
        description: text::CD,
    },
    Suggestion {
        text: "/sandbox status",
        description: text::SANDBOX_STATUS,
    },
    Suggestion {
        text: "/sandbox on",
        description: text::SANDBOX_ON,
    },
    Suggestion {
        text: "/sandbox off",
        description: text::SANDBOX_OFF,
    },
    Suggestion {
        text: "/model ",
        description: text::MODEL,
    },
    Suggestion {
        text: "/effort ",
        description: text::EFFORT,
    },
    Suggestion {
        text: "/thinking ",
        description: text::THINKING,
    },
    Suggestion {
        text: "/autocompact ",
        description: text::AUTOCOMPACT,
    },
    Suggestion {
        text: "/skills",
        description: text::SKILLS,
    },
    Suggestion {
        text: "/mcps",
        description: text::MCPS,
    },
    Suggestion {
        text: "/plugins",
        description: text::PLUGINS,
    },
    Suggestion {
        text: "/goals",
        description: text::GOALS,
    },
    Suggestion {
        text: "/goal create ",
        description: text::GOAL_CREATE,
    },
    Suggestion {
        text: "/goal plan ",
        description: text::GOAL_PLAN,
    },
    Suggestion {
        text: "/goal confirm ",
        description: text::GOAL_CONFIRM,
    },
    Suggestion {
        text: "/goal status ",
        description: text::GOAL_STATUS,
    },
    Suggestion {
        text: "/goal continue ",
        description: text::GOAL_CONTINUE,
    },
    Suggestion {
        text: "/goal start ",
        description: text::GOAL_START,
    },
    Suggestion {
        text: "/goal pause ",
        description: text::GOAL_PAUSE,
    },
    Suggestion {
        text: "/goal resume ",
        description: text::GOAL_RESUME,
    },
    Suggestion {
        text: "/goal cancel ",
        description: text::GOAL_CANCEL,
    },
    Suggestion {
        text: "/goal report ",
        description: text::GOAL_REPORT,
    },
    Suggestion {
        text: "/goal verify ",
        description: text::GOAL_VERIFY,
    },
    Suggestion {
        text: "/goal replan ",
        description: text::GOAL_REPLAN,
    },
];

/// Completion state for a single editor prefix. Updates perform no I/O.
#[derive(Debug, Default)]
pub struct Completion {
    pub items: Vec<Suggestion>,
    pub selected: usize,
    query: String,
    cursor: usize,
    enabled: bool,
    dismissed: bool,
}

impl Completion {
    /// Match the complete input prefix when the byte cursor is at its end.
    /// A changed prefix resets selection and dismissal. Repeated prefixes keep
    /// both, including while completion is temporarily disabled or hidden.
    pub fn update(&mut self, text: &str, cursor: usize, enabled: bool) {
        if self.query == text && self.cursor == cursor && self.enabled == enabled {
            return;
        }
        if self.query != text {
            self.query = text.to_owned();
            self.selected = 0;
            self.dismissed = false;
        }
        self.cursor = cursor;
        self.enabled = enabled;
        self.items.clear();
        if !self.eligible() {
            return;
        }
        self.items.extend(
            COMMANDS
                .iter()
                .copied()
                .filter(|suggestion| suggestion.text.starts_with(text)),
        );
        for command in ["/effort ", "/thinking "] {
            if text.starts_with(command) {
                self.items.extend(
                    ["low", "medium", "high", "xhigh", "max", "default"]
                        .into_iter()
                        .filter_map(|level| {
                            let suggestion = Suggestion {
                                text: match (command, level) {
                                    ("/effort ", "low") => "/effort low",
                                    ("/effort ", "medium") => "/effort medium",
                                    ("/effort ", "high") => "/effort high",
                                    ("/effort ", "xhigh") => "/effort xhigh",
                                    ("/effort ", "max") => "/effort max",
                                    ("/effort ", _) => "/effort default",
                                    (_, "low") => "/thinking low",
                                    (_, "medium") => "/thinking medium",
                                    (_, "high") => "/thinking high",
                                    (_, "xhigh") => "/thinking xhigh",
                                    (_, "max") => "/thinking max",
                                    _ => "/thinking default",
                                },
                                description: text::THINKING_LEVEL,
                            };
                            suggestion.text.starts_with(text).then_some(suggestion)
                        }),
                );
            }
        }
        if text.starts_with("/autocompact ") {
            self.items.extend(
                [
                    Suggestion {
                        text: "/autocompact auto",
                        description: text::AUTOCOMPACT_AUTO,
                    },
                    Suggestion {
                        text: "/autocompact off",
                        description: text::AUTOCOMPACT_OFF,
                    },
                ]
                .into_iter()
                .filter(|suggestion| suggestion.text.starts_with(text)),
            );
        }
        if matches!(text, "/effort " | "/thinking " | "/autocompact ") {
            self.items.retain(|suggestion| suggestion.text != text);
        }
        if self.items.len() == 1 && self.items[0].text == text {
            self.items.clear();
        }
    }

    /// Whether the current editor line accepts command completion, even when
    /// no catalog entry matches it or its suggestions have been dismissed.
    pub fn eligible(&self) -> bool {
        self.enabled
            && self.cursor == self.query.len()
            && self.query.starts_with('/')
            && !self.query.starts_with("//")
            && !self.query.contains(['\n', '\r'])
    }

    pub fn visible(&self) -> bool {
        self.eligible() && !self.dismissed && !self.items.is_empty()
    }

    pub fn current(&self) -> Option<&Suggestion> {
        if self.visible() {
            self.items.get(self.selected)
        } else {
            None
        }
    }

    /// Select the next visible item, wrapping to the first item.
    pub fn next(&mut self) {
        if self.visible() {
            let count = self.items.len();
            self.selected = (self.selected % count + 1) % count;
        }
    }

    /// Select the previous visible item, wrapping to the last item.
    pub fn previous(&mut self) {
        if self.visible() {
            let count = self.items.len();
            self.selected = (self.selected % count + count - 1) % count;
        }
    }

    pub fn dismiss(&mut self) {
        self.dismissed = true;
    }

    /// Reopen candidates for the current eligible prefix without changing it.
    pub fn reopen(&mut self) {
        self.dismissed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(completion: &Completion) -> Vec<&'static str> {
        completion.items.iter().map(|item| item.text).collect()
    }

    #[test]
    fn catalog_preserves_palette_order_and_adds_all_argument_scaffolds() {
        assert_eq!(COMMANDS.len(), 57);
        let expected = [
            "/mode plan",
            "/mode build",
            "/mode code",
            "/mode work",
            "/permissions auto",
            "/permissions manual",
            "/permissions bypass",
            "/sessions",
            "/new",
            "/profiles",
            "/tools",
            "/jobs",
            "/status",
            "/cancel",
            "/help",
            "/exit",
            "/ps",
            "/compact",
            "/memory",
            "/profile ",
            "/resume ",
            "/resume-accept ",
            "/cancel-job ",
            "/markdown render",
            "/markdown source",
            "/footnote ",
            "/footnote back",
            "/export json ",
            "/export markdown ",
            "/export full json ",
            "/export full markdown ",
            "/export-full json ",
            "/export-full markdown ",
            "/cd ",
            "/sandbox status",
            "/sandbox on",
            "/sandbox off",
            "/model ",
            "/effort ",
            "/thinking ",
            "/autocompact ",
            "/skills",
            "/mcps",
            "/plugins",
            "/goals",
            "/goal create ",
            "/goal plan ",
            "/goal confirm ",
            "/goal status ",
            "/goal continue ",
            "/goal start ",
            "/goal pause ",
            "/goal resume ",
            "/goal cancel ",
            "/goal report ",
            "/goal verify ",
            "/goal replan ",
        ];
        assert_eq!(
            COMMANDS.iter().map(|item| item.text).collect::<Vec<_>>(),
            expected
        );
        assert!(COMMANDS.iter().all(|item| !item.description.is_empty()));
    }

    #[test]
    fn empty_unknown_escaped_and_multiline_input_has_no_suggestions() {
        let mut completion = Completion::default();
        for text in [
            "",
            "plain text",
            " /mode",
            "//",
            "///",
            "//mode",
            "/unknown",
            "/mode\nplan",
            "/mode\rplan",
        ] {
            completion.update(text, text.len(), true);
            assert!(completion.items.is_empty(), "{text:?}");
            assert!(!completion.visible(), "{text:?}");
            assert!(completion.current().is_none(), "{text:?}");
        }
    }

    #[test]
    fn prefixes_match_the_whole_command_and_argument_scaffold() {
        let mut completion = Completion::default();
        completion.update("/", 1, true);
        assert_eq!(completion.items.len(), COMMANDS.len());
        completion.update("/mode ", 6, true);
        assert_eq!(
            texts(&completion),
            COMMANDS[..4]
                .iter()
                .map(|item| item.text)
                .collect::<Vec<_>>()
        );
        completion.update("/permissions m", 14, true);
        assert_eq!(texts(&completion), ["/permissions manual"]);
        completion.update("/export-full j", 14, true);
        assert_eq!(texts(&completion), ["/export-full json "]);
        completion.update("/export f", 9, true);
        assert_eq!(
            texts(&completion),
            ["/export full json ", "/export full markdown "]
        );
        completion.update("/resume", 7, true);
        assert_eq!(texts(&completion), ["/resume ", "/resume-accept "]);
        completion.update("/markdown ", 10, true);
        assert_eq!(texts(&completion), ["/markdown render", "/markdown source"]);
        completion.update("/thinking ", 10, true);
        assert_eq!(
            texts(&completion),
            [
                "/thinking low",
                "/thinking medium",
                "/thinking high",
                "/thinking xhigh",
                "/thinking max",
                "/thinking default",
            ]
        );
        completion.update("/effort h", 9, true);
        assert_eq!(texts(&completion), ["/effort high"]);
        completion.update("/autocompact ", 13, true);
        assert_eq!(
            texts(&completion),
            ["/autocompact auto", "/autocompact off"]
        );
        for text in ["/resume saved-id", "/export json result.json", "/MODE"] {
            completion.update(text, text.len(), true);
            assert!(!completion.visible(), "{text:?}");
        }
    }

    #[test]
    fn complete_commands_and_completed_scaffolds_hide_the_only_equal_match() {
        let mut completion = Completion::default();
        for suggestion in COMMANDS.iter().filter(|item| {
            !matches!(
                item.text,
                "/cancel" | "/footnote " | "/effort " | "/thinking " | "/autocompact "
            )
        }) {
            completion.update(suggestion.text, suggestion.text.len(), true);
            assert!(completion.eligible());
            assert!(!completion.visible(), "{:?}", suggestion.text);
            assert!(completion.current().is_none());
        }
        completion.update("/cancel", 7, true);
        assert_eq!(texts(&completion), ["/cancel", "/cancel-job "]);
        assert!(completion.visible());
        completion.update("/footnote ", 10, true);
        assert_eq!(texts(&completion), ["/footnote ", "/footnote back"]);
    }

    #[test]
    fn selection_wraps_and_repeated_prefixes_preserve_it() {
        let mut completion = Completion::default();
        completion.update("/m", 2, true);
        completion.previous();
        assert_eq!(completion.current().unwrap().text, "/mcps");
        completion.next();
        assert_eq!(completion.selected, 0);
        completion.next();
        completion.next();
        completion.update("/m", 2, true);
        assert_eq!(completion.current().unwrap().text, "/mode code");
        completion.update("/mo", 3, true);
        assert_eq!(completion.current().unwrap().text, "/mode plan");
    }

    #[test]
    fn dismissal_survives_updates_until_reopened_or_prefix_changes() {
        let mut completion = Completion::default();
        completion.update("/m", 2, true);
        completion.next();
        completion.dismiss();
        completion.update("/m", 2, true);
        completion.next();
        completion.previous();
        assert!(!completion.visible());
        assert!(completion.current().is_none());
        assert_eq!(completion.selected, 1);
        completion.reopen();
        assert_eq!(completion.current().unwrap().text, "/mode build");
        completion.dismiss();
        completion.update("/mo", 3, true);
        assert!(completion.visible());
        assert_eq!(completion.selected, 0);
    }

    #[test]
    fn disabled_completion_restores_the_current_prefix_and_dismissal() {
        let mut completion = Completion::default();
        completion.update("/m", 2, true);
        completion.next();
        completion.update("/m", 2, false);
        assert!(!completion.visible());
        assert!(completion.items.is_empty());
        completion.update("/m", 2, true);
        assert_eq!(completion.current().unwrap().text, "/mode build");
        completion.dismiss();
        completion.update("/m", 2, false);
        completion.update("/m", 2, true);
        assert!(!completion.visible());
        completion.update("/permissions ", 13, false);
        completion.update("/permissions ", 13, true);
        assert_eq!(completion.current().unwrap().text, "/permissions auto");
    }

    #[test]
    fn moving_the_cursor_hides_suggestions_without_losing_selection() {
        let mut completion = Completion::default();
        completion.update("/m", 2, true);
        completion.next();
        for cursor in [0, 1, 3, usize::MAX] {
            completion.update("/m", cursor, true);
            assert!(!completion.visible());
            assert!(completion.items.is_empty());
            assert_eq!(completion.selected, 1);
        }
        completion.update("/m", 2, true);
        assert_eq!(completion.current().unwrap().text, "/mode build");
        completion.update("/模式", 1, true);
        assert!(!completion.visible());
    }

    #[test]
    fn eligibility_distinguishes_unmatched_or_dismissed_commands_from_plain_text() {
        let mut completion = Completion::default();
        for text in ["/", "/unknown", "/mode plan", "/resume saved-id"] {
            completion.update(text, text.len(), true);
            completion.dismiss();
            assert!(completion.eligible(), "{text:?}");
            assert!(!completion.visible());
        }
        for text in ["", "ordinary text", "//escaped", "/mode\nplan"] {
            completion.update(text, text.len(), true);
            assert!(!completion.eligible(), "{text:?}");
        }
        completion.update("/m", 1, true);
        assert!(!completion.eligible());
        completion.update("/m", 2, false);
        assert!(!completion.eligible());
    }
}
