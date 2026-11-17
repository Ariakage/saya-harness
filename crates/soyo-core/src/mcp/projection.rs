use super::{McpContent, McpLimits, McpToolResult, error};
use crate::contract::BoundaryError;
use rmcp::model::{CallToolResult, ContentBlock, ResourceContents};

pub(super) fn project_result(
    result: CallToolResult,
    limits: &McpLimits,
) -> Result<McpToolResult, BoundaryError> {
    let wire_size = serde_json::to_vec(&result)
        .map_err(|_| error("mcp_protocol", "MCP tool output cannot be encoded"))?
        .len();
    if wire_size > limits.max_wire_bytes {
        return Err(error(
            "mcp_limit",
            "MCP tool output exceeds the wire-message limit",
        ));
    }
    let mut projected = McpToolResult {
        content: Vec::new(),
        structured_content: None,
        is_error: result.is_error.unwrap_or(false),
        truncated: result.content.len() > limits.max_content_blocks,
    };
    // Reserve the enclosing JSON fields and the punctuation between content items.
    let mut remaining = limits.max_result_bytes.saturating_sub(128);
    for block in result.content.into_iter().take(limits.max_content_blocks) {
        let mut item = match block {
            ContentBlock::Text(text) => McpContent::Text { text: text.text },
            ContentBlock::Image(image) => McpContent::Omitted {
                kind: "image".into(),
                mime_type: Some(image.mime_type),
                encoded_bytes: image.data.len(),
            },
            ContentBlock::Audio(audio) => McpContent::Omitted {
                kind: "audio".into(),
                mime_type: Some(audio.mime_type),
                encoded_bytes: audio.data.len(),
            },
            ContentBlock::Resource(resource) => match resource.resource {
                ResourceContents::TextResourceContents {
                    uri,
                    mime_type,
                    text,
                    ..
                } => McpContent::Resource {
                    uri,
                    mime_type,
                    text,
                },
                ResourceContents::BlobResourceContents {
                    mime_type, blob, ..
                } => McpContent::Omitted {
                    kind: "resource_blob".into(),
                    mime_type,
                    encoded_bytes: blob.len(),
                },
                _ => McpContent::Omitted {
                    kind: "unsupported_resource".into(),
                    mime_type: None,
                    encoded_bytes: 0,
                },
            },
            ContentBlock::ResourceLink(resource) => McpContent::ResourceLink {
                uri: resource.uri,
                name: resource.name,
                mime_type: resource.mime_type,
            },
            _ => McpContent::Omitted {
                kind: "unsupported".into(),
                mime_type: None,
                encoded_bytes: 0,
            },
        };
        cap_metadata(&mut item, &mut projected.truncated);
        loop {
            let length = serde_json::to_vec(&item)
                .map_err(|_| error("mcp_protocol", "MCP content cannot be encoded"))?
                .len()
                .saturating_add(1);
            if length <= remaining {
                remaining -= length;
                projected.content.push(item);
                break;
            }
            projected.truncated = true;
            match &mut item {
                McpContent::Text { text } | McpContent::Resource { text, .. }
                    if !text.is_empty() =>
                {
                    truncate(text, text.len() / 2);
                }
                _ => {
                    remaining = 0;
                    break;
                }
            }
        }
        if remaining == 0 {
            break;
        }
    }
    if let Some(structured) = result.structured_content {
        let bytes = serde_json::to_vec(&structured)
            .map_err(|_| error("mcp_protocol", "MCP structured content cannot be encoded"))?;
        if bytes.len() <= remaining {
            projected.structured_content = Some(structured);
        } else {
            projected.truncated = true;
        }
    }
    Ok(projected)
}

fn cap_metadata(content: &mut McpContent, truncated: &mut bool) {
    let mut cap = |text: &mut String, length: usize| {
        if text.len() > length {
            truncate(text, length);
            *truncated = true;
        }
    };
    match content {
        McpContent::Text { .. } => {}
        McpContent::Resource { uri, mime_type, .. } => {
            cap(uri, 2048);
            if let Some(mime) = mime_type {
                cap(mime, 256);
            }
        }
        McpContent::ResourceLink {
            uri,
            name,
            mime_type,
        } => {
            cap(uri, 2048);
            cap(name, 256);
            if let Some(mime) = mime_type {
                cap(mime, 256);
            }
        }
        McpContent::Omitted { mime_type, .. } => {
            if let Some(mime) = mime_type {
                cap(mime, 256);
            }
        }
    }
}

fn truncate(text: &mut String, mut length: usize) {
    while !text.is_char_boundary(length) {
        length -= 1;
    }
    text.truncate(length);
}

/// Capture decoded text and structured data before model projection caps.
/// Binary media is represented by an explicit omission descriptor.
pub(super) fn capture_text_result(
    result: &CallToolResult,
    store: &crate::output_store::OutputStore,
    scope: crate::output_store::OutputScope,
    max_bytes: usize,
) -> (
    Vec<crate::output_store::SealedOutput>,
    Vec<crate::contract::ErrorInfo>,
) {
    let captured = (|| {
        let mut capture = store.begin(scope, max_bytes)?;
        for (index, block) in result.content.iter().enumerate() {
            let value = match block {
                ContentBlock::Text(text) => {
                    serde_json::json!({"index":index,"kind":"text","text":text.text})
                }
                ContentBlock::Resource(resource) => match &resource.resource {
                    ResourceContents::TextResourceContents {
                        uri,
                        mime_type,
                        text,
                        ..
                    } => {
                        serde_json::json!({"index":index,"kind":"text_resource","uri":uri,"mime_type":mime_type,"text":text})
                    }
                    _ => serde_json::json!({"index":index,"kind":"omitted_binary_resource"}),
                },
                _ => serde_json::json!({"index":index,"kind":"omitted_non_text_content"}),
            };
            serde_json::to_writer(crate::output_store::CaptureWriter(&mut capture), &value)
                .map_err(|_| error("output_capture", "MCP text capture failed"))?;
            capture.push(b"\n")?;
        }
        if let Some(structured) = &result.structured_content {
            serde_json::to_writer(
                crate::output_store::CaptureWriter(&mut capture),
                &serde_json::json!({"kind":"structured_content","value":structured}),
            )
            .map_err(|_| error("output_capture", "MCP structured capture failed"))?;
            capture.push(b"\n")?;
        }
        capture.seal(crate::output_store::CaptureCompletion::Eof)
    })();
    match captured {
        Ok(output) => (vec![output], Vec::new()),
        Err(error) => (
            Vec::new(),
            vec![crate::contract::ErrorInfo {
                code: error.0.code,
                message: "MCP output capture is unavailable".into(),
            }],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn projection_describes_binary_and_preserves_structured_and_resource_text() {
        let result: CallToolResult = serde_json::from_value(json!({
            "content": [
                {"type":"image", "mimeType":"image/png", "data":"secret-base64"},
                {"type":"resource", "resource":{"uri":"file:///untrusted", "text":"resource text"}},
                {"type":"resource_link", "uri":"https://example.invalid/a", "name":"link"}
            ],
            "structuredContent":{"answer":42}, "isError":true,
            "_meta":{"secret":"must not be projected"}
        }))
        .unwrap();
        let result = project_result(result, &McpLimits::default()).unwrap();
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(result.is_error);
        assert!(!result.truncated);
        assert_eq!(result.structured_content, Some(json!({"answer":42})));
        assert!(serialized.contains("resource text"));
        assert!(!serialized.contains("secret-base64"));
        assert!(!serialized.contains("must not be projected"));
    }

    #[test]
    fn projection_caps_total_encoded_bytes_and_unicode_escaping() {
        let result: CallToolResult = serde_json::from_value(json!({
            "content":[{"type":"text","text":"😀\n\"\\".repeat(5000)}],
            "structuredContent":{"oversized":"a".repeat(2000)}
        }))
        .unwrap();
        let limits = McpLimits {
            max_result_bytes: 512,
            ..McpLimits::default()
        };
        let result = project_result(result, &limits).unwrap();
        assert!(result.truncated);
        assert!(serde_json::to_vec(&result).unwrap().len() <= 512);
    }
}
