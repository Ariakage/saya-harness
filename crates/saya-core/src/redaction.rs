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
/// Detect known values in text and nested JSON. Unsafe nesting is sensitive.
pub fn contains_secret(text: &str, secrets: &[String]) -> bool {
    if secrets.iter().all(String::is_empty) {
        return false;
    }
    if contains_raw(text, secrets) {
        return true;
    }
    let mut value = Value::String(text.to_owned());
    redact_inner(&mut value, secrets, 0).unwrap_or(true)
}
const MAX_REDACTION_DEPTH: usize = 256;
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
        let CheckedValue(value) = serde_json::from_str(text).map_err(|_| failure())?;
        Some(value)
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
    for candidate in secrets
        .iter()
        .flat_map(|secret| variants(secret, max_bytes))
    {
        if text.contains(&candidate) {
            *text = text.replace(&candidate, "[REDACTED]");
            changed = true;
        }
    }
    if contains_raw(text, secrets) {
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
    if contains_raw(&serialized, secrets) {
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
}
