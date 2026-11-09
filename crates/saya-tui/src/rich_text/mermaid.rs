//! Terminal-native Mermaid previews with whole-block source fallback.
use merman_core::{Engine, ParseOptions, diagram::RenderSemanticModel};
use mermansi::{ColorMode, MermansiOptions, OutputMode, render_model};
use saya_core::text::is_terminal_control;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MAX_SOURCE_BYTES: usize = 8192;
const MAX_SOURCE_LINES: usize = 128;
const MAX_SOURCE_TOKENS: usize = 512;
const MAX_NESTING: usize = 16;
const MAX_RENDER_WIDTH: usize = 240;
const MAX_RENDER_HEIGHT: usize = 128;
const MAX_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_GRAPHEME_BYTES: usize = 128;
const MAX_NODES: usize = 32;
const MAX_EDGES: usize = 64;
const MAX_DETAILS: usize = 128;

/// Render a complete Mermaid block without I/O or terminal escape sequences.
/// Invalid, unsupported, oversized and non-fitting diagrams return `None` so
/// the caller can retain the complete fenced source instead of a partial chart.
pub(super) fn render(source: &str, width: usize) -> Option<Vec<String>> {
    if width < 8 || source.len() > MAX_SOURCE_BYTES {
        return None;
    }
    let source = source.replace("\r\n", "\n").replace('\r', "\n");
    if !input_within_budget(&source) {
        return None;
    }
    let parsed = Engine::new()
        .parse_diagram_for_render_model_sync(&source, ParseOptions::strict())
        .ok()??;
    if !model_within_budget(&parsed.model) {
        return None;
    }
    let width = width.min(MAX_RENDER_WIDTH);
    let options = MermansiOptions::unicode()
        .with_color(ColorMode::Plain)
        .with_output_mode(OutputMode::Concise)
        .with_max_width(width)
        .with_max_height(MAX_RENDER_HEIGHT);
    let output = render_model(&parsed.model, &options).ok()?;
    if output.trim().is_empty()
        || output.len() > MAX_OUTPUT_BYTES
        || output
            .chars()
            .any(|ch| ch != '\n' && is_terminal_control(ch))
        || output
            .graphemes(true)
            .any(|cluster| cluster.len() > MAX_GRAPHEME_BYTES)
    {
        return None;
    }
    let lines = output.lines().collect::<Vec<_>>();
    if lines.len() > MAX_RENDER_HEIGHT
        || lines.iter().any(|line| {
            UnicodeWidthStr::width(*line) > width
                || (line.starts_with('[') && line.ends_with(" semantic model]"))
                || matches!(
                    *line,
                    "(no diagram detected)"
                        | "(empty render output)"
                        | "(empty flowchart)"
                        | "(empty sequence diagram)"
                )
        })
    {
        return None;
    }
    Some(lines.into_iter().map(str::to_owned).collect())
}

fn model_within_budget(model: &RenderSemanticModel) -> bool {
    let (nodes, edges, details) = match model {
        RenderSemanticModel::Flowchart(model) => (
            model.nodes.len().saturating_add(model.subgraphs.len()),
            model.edges.len(),
            model.class_defs.len(),
        ),
        RenderSemanticModel::Sequence(model) => (
            model.actors.len().saturating_add(model.boxes.len()),
            model.messages.len().saturating_add(model.notes.len()),
            model.actors.values().fold(0usize, |count, actor| {
                count
                    .saturating_add(actor.links.len())
                    .saturating_add(actor.properties.len())
            }),
        ),
        RenderSemanticModel::State(model) => (
            model.nodes.len(),
            model.edges.len(),
            model.states.len().saturating_add(model.links.len()),
        ),
        RenderSemanticModel::Class(model) => (
            model
                .classes
                .len()
                .saturating_add(model.notes.len())
                .saturating_add(model.interfaces.len())
                .saturating_add(model.namespaces.len()),
            model
                .relations
                .len()
                .saturating_add(model.notes.len())
                .saturating_add(model.interfaces.len()),
            model.classes.values().fold(0usize, |count, class| {
                count
                    .saturating_add(class.members.len())
                    .saturating_add(class.methods.len())
            }),
        ),
        RenderSemanticModel::Er(model) => (
            model.entities.len(),
            model.relationships.len(),
            model.entities.values().fold(0usize, |count, entity| {
                count.saturating_add(entity.attributes.len())
            }),
        ),
        _ => return false,
    };
    nodes > 0 && nodes <= MAX_NODES && edges <= MAX_EDGES && details <= MAX_DETAILS
}

fn input_within_budget(source: &str) -> bool {
    let trimmed = source.trim_start();
    if trimmed.is_empty()
        || source.len() > MAX_SOURCE_BYTES
        || has_decodable_label_markup(source)
        || matches!(trimmed.as_bytes().first(), Some(b'{') | Some(b'['))
        || source.lines().count() > MAX_SOURCE_LINES
        || source
            .chars()
            .any(|ch| !matches!(ch, '\n' | '\r' | '\t') && is_terminal_control(ch))
        || source
            .graphemes(true)
            .any(|cluster| cluster.len() > MAX_GRAPHEME_BYTES)
    {
        return false;
    }
    if let Some(frontmatter) = merman_core::preprocess::split_frontmatter_block(source)
        && has_yaml_reference(&frontmatter.dedented_body)
    {
        return false;
    }
    let mut tokens = 0;
    let mut word = false;
    let mut nesting = 0usize;
    let mut quote = false;
    let mut escaped = false;
    for character in source.chars() {
        if character.is_alphanumeric() || character == '_' {
            if !word {
                tokens += 1;
            }
            word = true;
        } else {
            word = false;
            tokens += usize::from(!character.is_whitespace());
        }
        if tokens > MAX_SOURCE_TOKENS {
            return false;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == '"' {
            quote = !quote;
        }
        if !quote {
            match character {
                '[' | '(' | '{' => {
                    nesting += 1;
                    if nesting > MAX_NESTING {
                        return false;
                    }
                }
                ']' | ')' | '}' => nesting = nesting.saturating_sub(1),
                _ => {}
            }
        }
    }
    true
}

// Entity decoding and label markup removal can reconstruct text after the caller's
// redaction pass. Keep those blocks as source instead of changing that boundary.
fn has_decodable_label_markup(source: &str) -> bool {
    source.as_bytes().windows(2).any(|pair| match pair[0] {
        b'&' => pair[1] == b'#' || pair[1].is_ascii_alphanumeric(),
        b'#' => pair[1].is_ascii_alphanumeric() || matches!(pair[1], b'+' | b'_'),
        _ => false,
    }) || ["ﬂ°", "¶ß", "**", "__"]
        .iter()
        .any(|marker| source.contains(marker))
        || source.split('<').skip(1).any(|rest| {
            rest.trim_start_matches(|ch: char| ch.is_whitespace() || ch == '/')
                .as_bytes()
                .first()
                .is_some_and(|ch| ch.is_ascii_alphabetic() || matches!(ch, b'!' | b'?'))
        })
}

// The upstream YAML builder clones aliases before the semantic model exists.
// Keep those sources on the code path instead of expanding aliases during UI rendering.
fn has_yaml_reference(yaml: &str) -> bool {
    let mut characters = yaml.chars().peekable();
    let mut previous = ' ';
    while let Some(character) = characters.next() {
        if matches!(character, '&' | '*')
            && (previous.is_whitespace() || "-?:,[]{}".contains(previous))
            && characters.peek().is_some_and(|next| !next.is_whitespace())
        {
            return true;
        }
        previous = character;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_unicode_flowcharts_deterministically_at_display_width() {
        let source = "flowchart LR\nA[开始] --> B[验证]\nB --> C[完成]";
        let first = render(source, 80).expect("small flowchart should fit");
        assert_eq!(Some(first.clone()), render(source, 80));
        let text = first.join("\n");
        for label in ["开始", "验证", "完成"] {
            assert!(text.contains(label));
        }
        assert!(text.chars().any(|ch| matches!(ch, '─' | '│' | '┌' | '┐')));
        assert!(
            first
                .iter()
                .all(|line| UnicodeWidthStr::width(line.as_str()) <= 80)
        );
    }

    #[test]
    fn preserves_branch_labels_and_sequence_messages() {
        let source = "flowchart TD\nA[Check] -->|yes| B[Build]\nA -->|no| C[Review]";
        let rendered = render(source, 100)
            .expect("small branch should fit")
            .join("\n");
        for label in ["Check", "yes", "no", "Build", "Review"] {
            assert!(rendered.contains(label), "missing {label}");
        }
        let source = "sequenceDiagram\nparticipant Alice\nparticipant Bob\nAlice->>Bob: Hello\nBob-->>Alice: Ready";
        let rendered = render(source, 100)
            .expect("small sequence should fit")
            .join("\n");
        for label in ["Alice", "Bob", "Hello", "Ready"] {
            assert!(rendered.contains(label), "missing {label}");
        }
    }

    #[test]
    fn malformed_empty_and_non_mermaid_sources_request_fallback() {
        for source in [
            "",
            "plain text",
            "flowchart TD\nA[unfinished",
            "sequenceDiagram\nAlice -?> Bob",
            "{\"name\":\"not Mermaid\"}",
            "[\"not Mermaid\"]",
        ] {
            assert!(
                render(source, 80).is_none(),
                "source was rendered: {source}"
            );
        }
    }

    #[test]
    fn small_windows_and_large_inputs_fall_back_as_complete_blocks() {
        assert!(render("flowchart TD\nA --> B", 0).is_none());
        assert!(render("flowchart TD\nA --> B", 1).is_none());
        assert!(!input_within_budget(&"x".repeat(MAX_SOURCE_BYTES + 1)));
        assert!(!input_within_budget(&format!(
            "flowchart TD\n{}",
            "A --> B\n".repeat(MAX_SOURCE_LINES)
        )));
        assert!(!input_within_budget(&format!(
            "flowchart TD\n{}",
            "A;".repeat(MAX_SOURCE_TOKENS)
        )));
        assert!(!input_within_budget(&format!(
            "flowchart TD\n{}",
            "[".repeat(MAX_NESTING + 1)
        )));
        assert!(input_within_budget(
            "flowchart TD\nA[\"quoted [[[ label\"] --> B"
        ));
    }

    #[test]
    fn control_characters_and_oversized_clusters_do_not_reach_the_renderer() {
        for marker in ['\u{1b}', '\u{7}', '\u{9b}', '\u{202e}', '\u{2066}'] {
            assert!(render(&format!("flowchart TD\nA[unsafe{marker}] --> B"), 80).is_none());
        }
        let cluster = format!("e{}", "\u{301}".repeat(MAX_GRAPHEME_BYTES));
        assert!(render(&format!("flowchart TD\nA[{cluster}] --> B"), 80).is_none());
    }

    #[test]
    fn supports_state_class_and_entity_relationship_families() {
        for (source, labels) in [
            (
                "stateDiagram-v2\n[*] --> Ready\nReady --> Done\nDone --> [*]",
                vec!["Ready", "Done"],
            ),
            (
                "classDiagram\nAnimal <|-- Duck\nAnimal : +String name",
                vec!["Animal", "Duck", "name"],
            ),
            (
                "erDiagram\nCUSTOMER ||--o{ ORDER : places",
                vec!["CUSTOMER", "ORDER", "places"],
            ),
        ] {
            let rendered = render(source, 100)
                .expect("small supported family should fit")
                .join("\n");
            for label in labels {
                assert!(rendered.contains(label), "missing {label}");
            }
        }
        assert!(render("pie title Unsupported family\n\"A\": 1", 80).is_none());
    }

    #[test]
    fn parsed_graph_limits_are_checked_before_layout() {
        let nodes = (0..=MAX_NODES)
            .map(|index| format!("N{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let source = format!("flowchart TD\n{nodes}");
        assert!(input_within_budget(&source));
        assert!(render(&source, 240).is_none());
        let edges = "A-->B\n".repeat(MAX_EDGES + 1);
        let source = format!("flowchart TD\n{edges}");
        assert!(input_within_budget(&source));
        assert!(render(&source, 240).is_none());
    }

    #[test]
    fn yaml_alias_expansion_requests_source_fallback_before_parsing() {
        let source = "---\nconfig:\n  first: &first [value, value]\n  second: &second [*first, *first]\n  third: [*second, *second]\n---\nflowchart TD\nA --> B";
        assert!(!input_within_budget(source));
        assert!(render(source, 80).is_none());
        assert!(render(&source.replace('\n', "\r"), 80).is_none());
        assert!(has_yaml_reference(
            "config: {source: &value [a, b], copy: *value}"
        ));
        assert!(has_yaml_reference(
            "title: John's diagram\nsource: &value [a, b]"
        ));
        assert!(!has_yaml_reference(
            "title: \"A * B & C\"\nother: 'literal text'"
        ));
    }

    #[test]
    fn entity_and_markup_decoding_request_source_fallback_before_parsing() {
        for label in [
            "s&#x6b;-SYNTHETIC",
            "s&#X6B;-SYNTHETIC",
            "s&#107;-SYNTHETIC",
            "s&#x6b-SYNTHETIC",
            "s&#107-SYNTHETIC",
            "a&amp;b",
            "a&amp b",
            "a&AMP;b",
            "a&AMP b",
            "a&1b",
            "s#107;-SYNTHETIC",
            "s#x6b;-SYNTHETIC",
            "a#amp;b",
            "sﬂ°°107¶ß-SYNTHETIC",
            "s<b>k</b>-SYNTHETIC",
            "s< b >k</ b >-SYNTHETIC",
            "s<strong>k</strong>-SYNTHETIC",
            "s**k**-SYNTHETIC",
            "s__k__-SYNTHETIC",
        ] {
            let source = format!("flowchart LR\nA[\"{label}\"] --> B[done]");
            assert!(!input_within_budget(&source), "preflight accepted {label}");
            assert!(render(&source, 120).is_none(), "rendered {label}");
        }
    }

    #[test]
    fn literal_ampersands_and_mermaid_arrows_remain_supported() {
        for source in [
            "flowchart LR\nA & B --> C",
            "flowchart LR\nA[\"A & B\"] --> C",
            "flowchart LR\nA <--> B",
            "classDiagram\nA <|-- B",
        ] {
            assert!(input_within_budget(source), "preflight rejected {source}");
            assert!(render(source, 120).is_some(), "failed to render {source}");
        }
    }
}
