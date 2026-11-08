//! Known-credential redaction for serialized data. Unknown secrets are not inferred.
use crate::contract::{BoundaryError, ErrorInfo, ToolResult};
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;

struct CheckedValue(Value);
impl<'de> Deserialize<'de> for CheckedValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct JsonVisitor;
        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = Value;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("JSON with unique object keys")
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
                Ok(value.into())
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
                Ok(value.into())
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
                Ok(value.into())
            }
            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
                serde_json::Number::from_f64(value)
                    .map(Value::Number)
                    .ok_or_else(|| E::custom("invalid JSON number"))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
                Ok(value.into())
            }
            fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
                Ok(value.into())
            }
            fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
                let mut values = Vec::new();
                while let Some(CheckedValue(value)) = sequence.next_element()? {
                    values.push(value);
                }
                Ok(Value::Array(values))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom("duplicate JSON object key"));
                    }
                    let CheckedValue(value) = map.next_value()?;
                    values.insert(key, value);
                }
                Ok(Value::Object(values))
            }
        }
        deserializer.deserialize_any(JsonVisitor).map(Self)
    }
}

fn failure() -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "sensitive_data".into(),
            message: "sensitive content could not be safely redacted".into(),
        },
        None,
        None,
    )
}
fn variants(secret: &str, max_bytes: usize) -> Vec<String> {
    let mut out = Vec::new();
    if secret.is_empty() {
        return out;
    }
    let mut candidate = secret.to_owned();
    while candidate.len() <= max_bytes {
        if out.contains(&candidate) {
            break;
        }
        out.push(candidate.clone());
        let Ok(encoded) = serde_json::to_string(&candidate) else {
            break;
        };
        let Some(fragment) = encoded.get(1..encoded.len().saturating_sub(1)) else {
            break;
        };
        candidate = fragment.to_owned();
    }
    out.sort_by_key(|item| std::cmp::Reverse(item.len()));
    out
}
fn contains_raw(text: &str, secrets: &[String]) -> bool {
    secrets
        .iter()
        .flat_map(|secret| variants(secret, text.len()))
        .any(|candidate| text.contains(&candidate))
}

// Match each credential character as either literal text or JSON escapes.
// Encoded backslashes may themselves use backslashes or Unicode escapes.
fn encoded_matcher(secrets: &[String]) -> Result<regex::RegexSet, regex::Error> {
    let slash = r"\\(?:\\|u005[cC])*";
    let patterns = secrets
        .iter()
        .filter(|secret| !secret.is_empty())
        .map(|secret| {
            let mut pattern = String::new();
            for character in secret.chars() {
                if character == '\\' {
                    pattern.push_str(slash);
                    continue;
                }
                let mut alternatives = vec![regex::escape(&character.to_string())];
                let short = match character {
                    '"' => Some('"'),
                    '/' => Some('/'),
                    '\n' => Some('n'),
                    '\r' => Some('r'),
                    '\t' => Some('t'),
                    '\u{8}' => Some('b'),
                    '\u{c}' => Some('f'),
                    _ => None,
                };
                if let Some(short) = short {
                    alternatives.push(format!("{slash}{}", regex::escape(&short.to_string())));
                }
                let mut units = [0u16; 2];
                let unicode = character
                    .encode_utf16(&mut units)
                    .iter()
                    .map(|unit| format!("{slash}u(?i:{unit:04x})"))
                    .collect::<String>();
                alternatives.push(unicode);
                pattern.push_str(&format!("(?:{})", alternatives.join("|")));
            }
            pattern
        });
    regex::RegexSetBuilder::new(patterns)
        .size_limit(MAX_ESCAPE_SCAN_BYTES)
        .build()
}

// Decode a detection-only view, including JSON escapes embedded in prose.
// A JSON escape occupies two bytes, six bytes, or a twelve-byte surrogate pair.
fn decode_text_escapes(text: &str, unicode_only: bool) -> Option<String> {
    let mut decoded = String::with_capacity(text.len());
    let mut remaining = text;
    let mut changed = false;
    while let Some(offset) = remaining.find('\\') {
        decoded.push_str(&remaining[..offset]);
        remaining = &remaining[offset..];
        let lengths: &[usize] = if unicode_only { &[6, 12] } else { &[2, 6, 12] };
        let escape = lengths.iter().copied().find_map(|length| {
            if unicode_only && !remaining.starts_with("\\u") {
                return None;
            }
            let fragment = remaining.get(..length)?;
            let value = serde_json::from_str::<String>(&format!("\"{fragment}\"")).ok()?;
            Some((length, value))
        });
        if let Some((length, value)) = escape {
            decoded.push_str(&value);
            remaining = &remaining[length..];
            changed = true;
        } else {
            decoded.push('\\');
            remaining = &remaining[1..];
        }
    }
    decoded.push_str(remaining);
    changed.then_some(decoded)
}

/// Detect known values in text and nested JSON, including escapes in prose.
/// Inspection preserves supplied text and fails closed on nesting or scan limits.
pub fn contains_secret(text: &str, secrets: &[String]) -> bool {
    if secrets.iter().all(String::is_empty) {
        return false;
    }
    if contains_raw(text, secrets) {
        return true;
    }
    if !text.contains('\\') {
        return false;
    }
    let Ok(matcher) = encoded_matcher(secrets) else {
        return true;
    };
    if matcher.is_match(text) {
        return true;
    }
    let mut remaining_scan = MAX_ESCAPE_SCAN_BYTES;
    inspect_escape_view(text, secrets, &matcher, &mut remaining_scan, true)
        || inspect_escape_view(text, secrets, &matcher, &mut remaining_scan, false)
}

fn inspect_escape_view(
    text: &str,
    secrets: &[String],
    matcher: &regex::RegexSet,
    remaining_scan: &mut usize,
    unicode_only: bool,
) -> bool {
    if !text.contains('\\') {
        return false;
    }
    let mut current = text.to_owned();
    for _ in 0..MAX_REDACTION_DEPTH {
        if !current.contains('\\') {
            return false;
        }
        let Some(remaining) = remaining_scan.checked_sub(current.len()) else {
            return true;
        };
        *remaining_scan = remaining;
        let Some(decoded) = decode_text_escapes(&current, unicode_only) else {
            return false;
        };
        if contains_raw(&decoded, secrets) || matcher.is_match(&decoded) {
            return true;
        }
        current = decoded;
    }
    current.contains('\\')
        && (current.len() > *remaining_scan
            || decode_text_escapes(&current, unicode_only).is_some())
}
const MAX_REDACTION_DEPTH: usize = 256;
const MAX_ESCAPE_SCAN_BYTES: usize = 64 * 1024 * 1024;
// Called only for validated JSON. Sorting permits object key reordering while
// retaining every numeric literal, including its original precision.
fn number_literals(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut offset = 0;
    let mut numbers = Vec::new();
    while offset < bytes.len() {
        if bytes[offset] == b'"' {
            offset += 1;
            while offset < bytes.len() {
                match bytes[offset] {
                    b'\\' => offset += 2,
                    b'"' => {
                        offset += 1;
                        break;
                    }
                    _ => offset += 1,
                }
            }
        } else if bytes[offset].is_ascii_digit() || bytes[offset] == b'-' {
            let start = offset;
            while offset < bytes.len()
                && matches!(
                    bytes[offset],
                    b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'
                )
            {
                offset += 1;
            }
            numbers.push(&text[start..offset]);
        } else {
            offset += 1;
        }
    }
    numbers.sort_unstable();
    numbers
}
fn redact_string(
    text: &mut String,
    secrets: &[String],
    depth: usize,
) -> Result<bool, BoundaryError> {
    if depth >= MAX_REDACTION_DEPTH {
        return Err(failure());
    }
    let mut changed = false;
    // IgnoredAny validates complete JSON without rounding or limiting numbers.
    // Plain text with a JSON-looking prefix must retain its ordinary semantics.
    let nested = if serde_json::from_str::<de::IgnoredAny>(text).is_ok() {
        match serde_json::from_str(text) {
            Ok(CheckedValue(value)) => Some(value),
            Err(_) if contains_secret(text, secrets) => return Err(failure()),
            Err(_) => None,
        }
    } else {
        None
    };
    if let Some(mut nested) = nested
        && !matches!(&nested, Value::String(inner) if inner == text)
        && redact_inner(&mut nested, secrets, depth + 1)?
    {
        let encoded = serde_json::to_string(&nested).map_err(|_| failure())?;
        if number_literals(text) != number_literals(&encoded) {
            return Err(failure());
        }
        *text = encoded;
        changed = true;
    }
    let max_bytes = text.len();
    let replacement = if contains_raw("[REDACTED]", secrets) {
        ""
    } else {
        "[REDACTED]"
    };
    for candidate in secrets
        .iter()
        .flat_map(|secret| variants(secret, max_bytes))
    {
        if text.contains(&candidate) {
            *text = text.replace(&candidate, replacement);
            changed = true;
        }
    }
    if contains_secret(text, secrets) {
        return Err(failure());
    }
    Ok(changed)
}
fn redact_inner(
    value: &mut Value,
    secrets: &[String],
    depth: usize,
) -> Result<bool, BoundaryError> {
    if depth >= MAX_REDACTION_DEPTH {
        return Err(failure());
    }
    let mut changed = false;
    match value {
        Value::String(text) => changed = redact_string(text, secrets, depth)?,
        Value::Array(values) => {
            for item in values {
                changed |= redact_inner(item, secrets, depth + 1)?;
            }
        }
        Value::Object(values) => {
            let original = std::mem::take(values);
            for (mut key, mut item) in original {
                changed |= redact_string(&mut key, secrets, depth + 1)?;
                changed |= redact_inner(&mut item, secrets, depth + 1)?;
                if values.insert(key, item).is_some() {
                    return Err(failure());
                }
            }
        }
        _ => {}
    }
    Ok(changed)
}
/// Redact recursively, including nested JSON and escaped credential variants.
/// Returns an error instead of leaking or silently merging object keys.
pub fn redact(value: &mut Value, secrets: &[String]) -> Result<(), BoundaryError> {
    if secrets.iter().all(String::is_empty) {
        return Ok(());
    }
    redact_inner(value, secrets, 0)?;
    let serialized = serde_json::to_string(value).map_err(|_| failure())?;
    if contains_secret(&serialized, secrets) {
        return Err(failure());
    }
    Ok(())
}
/// Return a sanitized tool result, preserving its schema.
pub fn tool_result(result: &ToolResult, secrets: &[String]) -> Result<ToolResult, BoundaryError> {
    let mut value = serde_json::to_value(result).map_err(|_| failure())?;
    redact(&mut value, secrets)?;
    serde_json::from_value(value).map_err(|_| failure())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn encoded_credentials_in_prose_fail_closed_for_redaction_and_tool_results() {
        let known = ["fixture-secret".to_owned()];
        for text in [
            r#"Notes: {"token":"fixture-\u0073ecret"}"#,
            "```json\n{\"token\":\"fixture-\\u0073ecret\"}\n```",
            "{\"safe\":1}\n{\"token\":\"fixture-\\u0073ecret\"}",
        ] {
            assert!(contains_secret(text, &known));
            assert!(redact(&mut Value::String(text.into()), &known).is_err());
            let result = ToolResult {
                ok: true,
                data: Some(json!({"text":text})),
                error: None,
                truncated: false,
            };
            assert!(tool_result(&result, &known).is_err());
        }
    }

    #[test]
    fn partial_unicode_encoding_keeps_literal_backslash_credentials_detectable() {
        let known = [r"key\tail1".to_owned()];
        for text in [r"Note: key\tail\u0031", r"Note: key\tail\u005cu0031"] {
            assert!(contains_secret(text, &known));
            assert!(redact(&mut Value::String(text.into()), &known).is_err());
        }
        for (secret, text) in [
            ("key\\tail\"end", r#"Note: key\tail\"end"#),
            ("key\\tail\nend", r"Note: key\tail\nend"),
            ("key\\tail\"end", r#"Note: key\tail\\\"end"#),
        ] {
            let known = [secret.to_owned()];
            assert!(contains_secret(text, &known), "{text:?}");
            assert!(redact(&mut Value::String(text.into()), &known).is_err());
        }
    }

    #[test]
    fn harmless_opaque_json_and_unescaped_large_text_are_preserved() {
        let known = ["fixture-secret".to_owned()];
        for text in [r#"{"value":1,"value":2}"#, r#"{"value":1e400}"#] {
            let mut value = Value::String(text.into());
            redact(&mut value, &known).unwrap();
            assert_eq!(value, Value::String(text.into()));
            assert!(!contains_secret(text, &known));
        }
        assert!(!contains_secret(
            &"x".repeat(MAX_ESCAPE_SCAN_BYTES + 1),
            &known
        ));
        for secret in ["REDACTED", "[REDACTED]"] {
            let known = [secret.to_owned()];
            let mut value = Value::String(format!("before {secret} after"));
            redact(&mut value, &known).unwrap();
            assert!(!contains_secret(value.as_str().unwrap(), &known));
        }
    }

    #[test]
    fn redacts_nested_json_and_escaped_credentials_without_collisions() {
        let secret = "fixture\"secret\\tail".to_string();
        let inner = json!({"value":secret}).to_string();
        let mut value = json!({"outer":inner});
        redact(&mut value, std::slice::from_ref(&secret)).unwrap();
        assert!(!contains_secret(
            &value.to_string(),
            std::slice::from_ref(&secret)
        ));
        assert!(value.to_string().contains("[REDACTED]"));
        let mut collision = json!({"token":"one","[REDACTED]":"two"});
        assert!(redact(&mut collision, &["token".into()]).is_err());
    }

    #[test]
    fn inspects_deep_and_unicode_encoded_json_and_preserves_safe_text() {
        let secret = "fixture\"secret\\tail".to_owned();
        let unicode = r#"{"token":"fixture\u0022secret\u005ctail"}"#;
        assert_eq!(
            serde_json::from_str::<Value>(unicode).unwrap()["token"],
            secret
        );
        for payload in [json!({"token":secret}).to_string(), unicode.to_owned()] {
            let mut nested = payload;
            for _ in 0..12 {
                assert!(contains_secret(&nested, std::slice::from_ref(&secret)));
                let mut value = json!({"payload":nested});
                redact(&mut value, std::slice::from_ref(&secret)).unwrap();
                assert!(!contains_secret(
                    &value.to_string(),
                    std::slice::from_ref(&secret)
                ));
                nested = serde_json::to_string(&nested).unwrap();
            }
        }
        let mut safe = json!({"payload":" { \"safe\" : [1, 2] } ", "text":"ordinary text", "hash":"1e400suffix"});
        let original = safe.clone();
        redact(&mut safe, std::slice::from_ref(&secret)).unwrap();
        assert_eq!(safe, original);
        let too_deep = format!("{}{}{}", "[".repeat(129), unicode, "]".repeat(129));
        assert!(contains_secret(&too_deep, std::slice::from_ref(&secret)));
        assert!(redact(&mut Value::String(too_deep), std::slice::from_ref(&secret)).is_err());
        let duplicate = r#"{"token":"fixture\u0022secret\u005ctail","token":"safe"}"#;
        assert!(contains_secret(duplicate, std::slice::from_ref(&secret)));
        assert!(
            redact(
                &mut Value::String(duplicate.to_owned()),
                std::slice::from_ref(&secret)
            )
            .is_err()
        );
        for number in [
            "1e400",
            "18446744073709551617",
            "1e-9999",
            "0.123456789012345678901",
        ] {
            let payload = format!(r#"{{"token":"fixture\u0022secret\u005ctail","n":{number}}}"#);
            assert!(contains_secret(&payload, std::slice::from_ref(&secret)));
            assert!(redact(&mut Value::String(payload), std::slice::from_ref(&secret)).is_err());
        }
        let mut precise =
            Value::String(r#"{"z":2.5,"token":"fixture\u0022secret\u005ctail","a":1}"#.to_owned());
        redact(&mut precise, &[secret]).unwrap();
        let Value::String(precise) = precise else {
            panic!("string expected")
        };
        let precise: Value = serde_json::from_str(&precise).unwrap();
        assert_eq!(precise["a"], 1);
        assert_eq!(precise["z"], 2.5);
        assert_eq!(precise["token"], "[REDACTED]");
    }

    #[test]
    fn detects_json_escapes_embedded_in_prose_and_markdown() {
        for (secret, payload) in [
            (
                "fixture\"secret\\tail",
                r#"{"token":"fixture\u0022secret\u005ctail"}"#,
            ),
            ("a🌸z", r#"{"token":"a\uD83C\uDF38z"}"#),
            ("fixture/secret\ttail\n", r#"fi\u0078ture\/secret\ttail\n"#),
        ] {
            let known = [secret.to_owned()];
            let mut encoded = payload.to_owned();
            for _ in 0..4 {
                for wrapped in [
                    format!("# Memory\n\n```json\n{encoded}\n```\n"),
                    format!("Saved example: {encoded}\nEnd of note."),
                    format!("\\q {encoded} \\uD83C tail"),
                ] {
                    assert!(contains_secret(&wrapped, &known));
                }
                encoded = serde_json::to_string(&encoded).unwrap();
            }
        }
    }

    #[test]
    fn escaped_prose_detection_is_bounded_and_preserves_safe_documents() {
        let known = ["fixture-secret".to_owned()];
        let safe = "# Memory\n```json\n{\"emoji\":\"\\uD83C\\uDF38\",\"n\":18446744073709551617}\n```\n\\q \\uD83C \\uZZZZ 路径\\notes";
        assert!(!contains_secret(safe, &known));
        let mut value = Value::String(safe.to_owned());
        redact(&mut value, &known).unwrap();
        assert_eq!(value, Value::String(safe.to_owned()));
        let deep = format!("Note: \\{}u0061", "u005c".repeat(MAX_REDACTION_DEPTH + 1));
        assert!(contains_secret(&deep, &known));
        assert!(!contains_secret(&deep, &[]));
        let at_limit = format!("Note: \\{}u0061", "u005c".repeat(MAX_REDACTION_DEPTH - 1));
        assert!(!contains_secret(&at_limit, &known));
        let padded = format!(
            "Note: \\{}u0061\n{}",
            "u005c".repeat(64),
            "x".repeat(MAX_ESCAPE_SCAN_BYTES / 32)
        );
        assert!(contains_secret(&padded, &known));
    }
}
