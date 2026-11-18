//! Centralized credential redaction helpers for IPC payloads and titles.

use serde_json::Value;
use soyo_core::redaction;

const WITHHELD: &str = "sensitive details withheld";

pub fn redact_str(raw: &str, secrets: &[String]) -> String {
    if secrets.is_empty() {
        return raw.to_owned();
    }
    let mut value = Value::String(raw.to_owned());
    if redaction::redact(&mut value, secrets).is_ok() {
        value.as_str().unwrap_or(WITHHELD).to_owned()
    } else {
        WITHHELD.to_owned()
    }
}

pub fn redact_value(raw: &Value, secrets: &[String]) -> Value {
    if secrets.is_empty() {
        return raw.clone();
    }
    let mut value = raw.clone();
    if redaction::redact(&mut value, secrets).is_ok()
        && !redaction::contains_secret(&value.to_string(), secrets)
    {
        value
    } else {
        Value::String(WITHHELD.into())
    }
}

pub fn safe_title(raw: &str, secrets: &[String]) -> String {
    let collapsed = raw
        .chars()
        .filter(|ch| !ch.is_control() || *ch == '\n' || *ch == '\t' || *ch == ' ')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let truncated: String = collapsed.chars().take(80).collect();
    let redacted = redact_str(&truncated, secrets);
    if redaction::contains_secret(&redacted, secrets) {
        WITHHELD.to_owned()
    } else {
        redacted
    }
}
