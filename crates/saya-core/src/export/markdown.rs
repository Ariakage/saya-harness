use super::{SessionExport, error};
use crate::contract::BoundaryError;
use serde::Serialize;

struct Markdown {
    text: String,
    limit: usize,
}
impl Markdown {
    fn append(&mut self, text: &str) -> Result<(), BoundaryError> {
        if self.text.len().saturating_add(text.len()) > self.limit {
            return Err(error(
                "export_limit",
                "Markdown export exceeds output limit",
            ));
        }
        self.text.push_str(text);
        Ok(())
    }
    fn fence(&mut self, text: &str, language: &str) -> Result<(), BoundaryError> {
        let text = safe_text(text);
        let length = text
            .split(|c| c != '`')
            .map(str::len)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(3);
        let fence = "`".repeat(length);
        self.append(&fence)?;
        self.append(language)?;
        self.append("\n")?;
        self.append(&text)?;
        self.append("\n")?;
        self.append(&fence)?;
        self.append("\n\n")
    }
    fn section(&mut self, heading: &str, value: &impl Serialize) -> Result<(), BoundaryError> {
        self.append("## ")?;
        self.append(heading)?;
        self.append("\n\n")?;
        let text = serde_json::to_string_pretty(value)
            .map_err(|_| error("export_error", "cannot encode Markdown section"))?;
        self.fence(&text, "json")
    }
}
fn safe_text(text: &str) -> String {
    let mut safe = String::new();
    for character in text.chars() {
        match character {
            '&' => safe.push_str("&amp;"),
            '<' => safe.push_str("&lt;"),
            '>' => safe.push_str("&gt;"),
            '\n' | '\t' => safe.push(character),
            character
                if character.is_control()
                    || matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') =>
            {
                use std::fmt::Write;
                // Writing into a String has no fallible external sink.
                let _ = write!(safe, "\\u{{{:x}}}", character as u32);
            }
            character => safe.push(character),
        }
    }
    safe
}
impl SessionExport {
    /// Render the same sanitized projection as JSON. Untrusted text stays in
    /// a dynamically sized fence, with explicit control/HTML escaping. The
    /// output is bounded and does not mutate or re-read the source session.
    pub fn to_markdown(&self) -> Result<Vec<u8>, BoundaryError> {
        let mut markdown = Markdown {
            text: String::new(),
            limit: self.max_output_bytes,
        };
        markdown.append("# Saya session export\n\n")?;
        markdown.section("Snapshot", &self.snapshot)?;
        markdown.section("Session", &self.session)?;
        markdown.append("## Conversation\n\n")?;
        for item in &self.history {
            let label = match item["kind"].as_str() {
                Some("user_text") => "User",
                Some("assistant_text") => "Assistant",
                Some("assistant_partial") => "Assistant (partial)",
                Some("tool_call") => "Tool call",
                Some("tool_result") => "Tool result",
                _ => "Stored item",
            };
            markdown.append("### ")?;
            markdown.append(label)?;
            if let Some(ordinal) = item["ordinal"].as_u64() {
                markdown.append(&format!(" · {ordinal}"))?;
            }
            markdown.append("\n\n")?;
            if let Some(text) = item["text"].as_str() {
                markdown.fence(text, "text")?;
            } else {
                markdown.fence(
                    &serde_json::to_string_pretty(item)
                        .map_err(|_| error("export_error", "cannot encode history item"))?,
                    "json",
                )?;
            }
        }
        markdown.section("Runs", &self.runs)?;
        markdown.section("Tool calls", &self.tool_calls)?;
        markdown.section("Background jobs", &self.background_jobs)?;
        markdown.section("Context sources", &self.project_context)?;
        if self.snapshot.scope == super::ExportScope::Full {
            markdown.section("Metrics", &self.metrics)?;
            markdown.section("Events", &self.events)?;
            markdown.section("Activities", &self.activities)?;
            markdown.section("Approval requests", &self.approval_requests)?;
        }
        markdown.section("Omitted fields", &self.omitted_fields)?;
        markdown.section("Truncated fields", &self.truncated_fields)?;
        Ok(markdown.text.into_bytes())
    }
}
