use super::*;

pub(super) fn expected_worker_schemas() -> Vec<(&'static str, Value)> {
    vec![
        (
            "browser_navigate",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "additionalProperties": false,
                "properties": {"url": {"description": "The URL to navigate to", "type": "string"}},
                "required": ["url"],
                "type": "object"
            }),
        ),
        (
            "browser_snapshot",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "additionalProperties": false,
                "properties": {
                    "boxes": {"description": "Include each element's bounding box as [box=x,y,width,height] in the snapshot. Coordinates are viewport-relative, in CSS pixels (Element.getBoundingClientRect)", "type": "boolean"},
                    "depth": {"description": "Limit the depth of the snapshot tree", "type": "number"},
                    "filename": {"description": "Save snapshot to a file instead of returning it in the response. Relative file names are resolved against the workspace root.", "type": "string"},
                    "target": {"description": "Exact target element reference from the page snapshot, or a unique element selector", "type": "string"}
                },
                "type": "object"
            }),
        ),
        (
            "browser_click",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "additionalProperties": false,
                "properties": {
                    "button": {"description": "Button to click, defaults to left", "enum": ["left", "right", "middle"], "type": "string"},
                    "doubleClick": {"description": "Whether to perform a double click instead of a single click", "type": "boolean"},
                    "element": {"description": "Human-readable element description used to obtain permission to interact with the element", "type": "string"},
                    "modifiers": {"description": "Modifier keys to press", "items": {"enum": ["Alt", "Control", "ControlOrMeta", "Meta", "Shift"], "type": "string"}, "type": "array"},
                    "target": {"description": "Exact target element reference from the page snapshot, or a unique element selector", "type": "string"}
                },
                "required": ["target"],
                "type": "object"
            }),
        ),
        (
            "browser_type",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "additionalProperties": false,
                "properties": {
                    "element": {"description": "Human-readable element description used to obtain permission to interact with the element", "type": "string"},
                    "slowly": {"description": "Whether to type one character at a time. Useful for triggering key handlers in the page. By default entire text is filled in at once.", "type": "boolean"},
                    "submit": {"description": "Whether to submit entered text (press Enter after)", "type": "boolean"},
                    "target": {"description": "Exact target element reference from the page snapshot, or a unique element selector", "type": "string"},
                    "text": {"description": "Text to type into the element", "type": "string"}
                },
                "required": ["target", "text"],
                "type": "object"
            }),
        ),
        (
            "browser_press_key",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "additionalProperties": false,
                "properties": {"key": {"description": "Name of the key to press or a character to generate, such as `ArrowLeft` or `a`", "type": "string"}},
                "required": ["key"],
                "type": "object"
            }),
        ),
        (
            "browser_wait_for",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "additionalProperties": false,
                "properties": {
                    "text": {"description": "The text to wait for", "type": "string"},
                    "textGone": {"description": "The text to wait for to disappear", "type": "string"},
                    "time": {"description": "The time to wait in seconds, at most 30. When combined with text or textGone, serves as a timeout for them instead of the default action timeout", "type": "number"}
                },
                "type": "object"
            }),
        ),
        (
            // The pinned worker exposes no masking parameter. This schema is
            // pinned so identity checks are exact and so the absence of a mask
            // input is an explicit capability boundary, not an accident.
            "browser_take_screenshot",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "additionalProperties": false,
                "properties": {
                    "element": {"description": "Human-readable element description used to obtain permission to interact with the element", "type": "string"},
                    "target": {"description": "Exact target element reference from the page snapshot, or a unique element selector", "type": "string"},
                    "type": {"description": "Image format for the screenshot. If unset, inferred from the filename extension, otherwise png.", "enum": ["png", "jpeg", "webp"], "type": "string"},
                    "filename": {"description": "File name to save the screenshot to. Relative file names are resolved against the workspace root. If not specified, the screenshot is saved into the output directory as `page-{timestamp}.{png|jpeg|webp}`.", "type": "string"},
                    "fullPage": {"description": "When true, takes a screenshot of the full scrollable page, instead of the currently visible viewport. Cannot be used with element screenshots.", "type": "boolean"},
                    "scale": {"default": "css", "description": "Image resolution scale. \"css\" produces a screenshot sized in CSS pixels (smaller, consistent across devices). \"device\" produces a high-resolution screenshot using device pixels (larger, accounts for the device pixel ratio). Default is css.", "enum": ["css", "device"], "type": "string"}
                },
                "required": ["scale"],
                "type": "object"
            }),
        ),
        (
            "browser_close",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "additionalProperties": false,
                "properties": {},
                "type": "object"
            }),
        ),
    ]
}

pub(super) fn validate_catalog(catalog: &McpCatalog) -> Result<(), BrowserFailure> {
    let expected = expected_worker_schemas();
    let mapped = MAPPED_WORKER_TOOLS.iter().copied().collect::<BTreeSet<_>>();
    for (name, schema) in &expected {
        let tool = catalog
            .tools
            .iter()
            .find(|tool| tool.name == *name)
            .ok_or_else(|| {
                BrowserFailure::new(
                    "worker_catalog",
                    "Browser worker is missing a required tool",
                )
            })?;
        if tool.input_schema != *schema {
            return Err(BrowserFailure::new(
                "worker_catalog",
                "Browser worker tool schema differs from the pinned schema",
            ));
        }
        if !mapped.contains(name) {
            return Err(BrowserFailure::new(
                "worker_catalog",
                "Browser worker tool mapping is not pinned",
            ));
        }
    }
    // The file-input tool is mapped for the host file broker. Its exact
    // descriptive text is not part of the host contract, but the argument
    // shape must still be the pinned `paths: string[]` form.
    let upload = catalog
        .tools
        .iter()
        .find(|tool| tool.name == "browser_file_upload")
        .ok_or_else(|| {
            BrowserFailure::new(
                "worker_catalog",
                "Browser worker is missing its file upload tool",
            )
        })?;
    let paths = upload
        .input_schema
        .get("properties")
        .and_then(|properties| properties.get("paths"));
    if !paths.is_some_and(|paths| paths.get("type").and_then(Value::as_str) == Some("array")) {
        return Err(BrowserFailure::new(
            "worker_catalog",
            "Browser worker file upload schema is not the pinned shape",
        ));
    }
    Ok(())
}

pub(super) fn first_text(result: &McpToolResult) -> Result<&str, BrowserFailure> {
    result
        .content
        .iter()
        .find_map(|content| match content {
            crate::mcp::McpContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .ok_or_else(|| {
            BrowserFailure::new(
                "invalid_provider_result",
                "Browser worker result did not contain text",
            )
        })
}

fn section_body<'a>(text: &'a str, heading: &str) -> Option<Vec<&'a str>> {
    let mut lines = text.lines();
    let mut started = false;
    let mut body = Vec::new();
    for line in lines.by_ref() {
        let line = line.trim_end_matches('\r');
        if !started {
            if line == heading {
                started = true;
            }
            continue;
        }
        if line.starts_with("### ") {
            break;
        }
        body.push(line);
    }
    started.then_some(body)
}

pub(super) fn page_fields(text: &str) -> (Option<String>, Option<String>) {
    let Some(lines) = section_body(text, "### Page") else {
        return (None, None);
    };
    let mut url = None;
    let mut title = None;
    for line in lines {
        if let Some(value) = line.strip_prefix("- Page URL: ")
            && url.is_none()
        {
            url = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("- Page Title: ")
            && title.is_none()
        {
            title = Some(value.to_owned());
        }
    }
    (url, title)
}

pub(super) fn result_detail(text: &str) -> Option<String> {
    section_body(text, "### Result").and_then(|lines| {
        lines
            .into_iter()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_owned)
    })
}

/// Playwright writes its own snapshot files into the same output directory.
/// They are not downloads and must not participate in the directory diff.
pub(super) fn worker_metadata_file(name: &str) -> bool {
    name.starts_with("page-") && (name.ends_with(".yml") || name.ends_with(".yaml"))
}

/// Extract the worker's own download event from the fixed `### Download`
/// section. Page titles, snapshot bodies and prose elsewhere in the result are
/// never searched for a path. Exactly one well-formed event is accepted.
pub(super) fn download_path(text: &str) -> Result<Option<String>, BrowserFailure> {
    let Some(lines) = section_body(text, "### Download") else {
        return Ok(None);
    };
    let mut paths = Vec::new();
    for line in lines {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        let Some(rest) = line.strip_prefix("- Downloaded file ") else {
            continue;
        };
        let Some(start) = rest.rfind(" to \"") else {
            continue;
        };
        let path = &rest[start + 5..];
        let Some(path) = path.strip_suffix('"') else {
            continue;
        };
        if path.is_empty()
            || path.len() > 4096
            || path.chars().any(char::is_control)
            || path.contains('\0')
        {
            continue;
        }
        paths.push(path.to_owned());
    }
    match paths.len() {
        0 => Ok(None),
        1 => Ok(paths.pop()),
        _ => Err(BrowserFailure::new(
            "invalid_provider_result",
            "Browser download section names more than one file",
        )),
    }
}

fn snapshot_yaml(text: &str) -> Option<String> {
    let lines = section_body(text, "### Snapshot")?;
    let mut body = Vec::new();
    let mut in_yaml = false;
    for line in lines {
        if !in_yaml {
            if line.starts_with("```yaml") {
                in_yaml = true;
            }
            continue;
        }
        if line.starts_with("```") {
            return Some(body.join("\n"));
        }
        body.push(line);
    }
    None
}

const MAX_ARIA_LINE_BYTES: usize = 16 * 1024;
const MAX_ARIA_NAME_BYTES: usize = 4096;
const MAX_ARIA_REF_BYTES: usize = 1024;

fn invalid_snapshot(message: &'static str) -> BrowserFailure {
    BrowserFailure::new("invalid_provider_result", message)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ElementReference {
    Main,
    Frame(String),
}

/// Parse exactly the reference grammar emitted by the pinned provider:
/// `e<digits>` for the main document and `f<digits>e<digits>` for an element
/// owned by a child frame. A malformed value fails the whole snapshot instead
/// of being ignored with its subtree.
fn parse_element_reference(key: &str) -> Result<ElementReference, BrowserFailure> {
    if key.is_empty() || key.len() > MAX_ARIA_REF_BYTES {
        return Err(invalid_snapshot(
            "Browser snapshot contains an invalid element reference",
        ));
    }
    let bytes = key.as_bytes();
    let mut index = 0usize;
    let mut frame = None;
    if bytes[0] == b'f' {
        let start = index;
        index += 1;
        let digits = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index == digits {
            return Err(invalid_snapshot(
                "Browser snapshot contains an invalid frame reference",
            ));
        }
        frame = Some(key[start..index].to_owned());
    }
    if bytes.get(index) != Some(&b'e') {
        return Err(invalid_snapshot(
            "Browser snapshot contains an invalid element reference",
        ));
    }
    index += 1;
    let digits = index;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index == digits || index != bytes.len() {
        return Err(invalid_snapshot(
            "Browser snapshot contains an invalid element reference",
        ));
    }
    Ok(match frame {
        Some(frame) => ElementReference::Frame(frame),
        None => ElementReference::Main,
    })
}

/// Decode one JSON (double-quoted) string and return the remaining input.
/// Accessible names are always JSON-encoded by the provider, so a bracket that
/// looks like `[ref=...]` inside the name is data, never a key.
fn parse_quoted(input: &str) -> Result<(String, &str), BrowserFailure> {
    if !input.starts_with('"') {
        return Err(invalid_snapshot(
            "Browser snapshot name is not a quoted string",
        ));
    }
    let tokens: Vec<(usize, char)> = input.char_indices().collect();
    let mut position = 1usize;
    let mut output = String::new();
    while position < tokens.len() {
        let (_, character) = tokens[position];
        position += 1;
        match character {
            '"' => {
                if output.len() > MAX_ARIA_NAME_BYTES {
                    return Err(invalid_snapshot("Browser snapshot name is too long"));
                }
                let rest = tokens.get(position).map_or("", |(byte, _)| &input[*byte..]);
                return Ok((output, rest));
            }
            '\\' => {
                let Some((_, escape)) = tokens.get(position).copied() else {
                    return Err(invalid_snapshot(
                        "Browser snapshot name has an incomplete escape",
                    ));
                };
                position += 1;
                match escape {
                    '"' => output.push('"'),
                    '\\' => output.push('\\'),
                    '/' => output.push('/'),
                    'b' => output.push('\u{8}'),
                    'f' => output.push('\u{c}'),
                    'n' => output.push('\n'),
                    'r' => output.push('\r'),
                    't' => output.push('\t'),
                    'u' => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            let Some((_, digit)) = tokens.get(position).copied() else {
                                return Err(invalid_snapshot(
                                    "Browser snapshot name has an incomplete unicode escape",
                                ));
                            };
                            position += 1;
                            let Some(value) = digit.to_digit(16) else {
                                return Err(invalid_snapshot(
                                    "Browser snapshot name has an invalid unicode escape",
                                ));
                            };
                            code = code.saturating_mul(16).saturating_add(value);
                        }
                        let mut scalar = code;
                        if (0xd800..=0xdbff).contains(&code) {
                            let pair = tokens.get(position..position.saturating_add(6));
                            let mut paired = false;
                            if let Some(pair) = pair
                                && pair[0].1 == '\\'
                                && pair[1].1 == 'u'
                            {
                                let mut low = 0u32;
                                let mut valid = true;
                                for (_, digit) in &pair[2..] {
                                    match digit.to_digit(16) {
                                        Some(value) => {
                                            low = low.saturating_mul(16).saturating_add(value)
                                        }
                                        None => {
                                            valid = false;
                                            break;
                                        }
                                    }
                                }
                                if valid && (0xdc00..=0xdfff).contains(&low) {
                                    scalar = 0x10000u32
                                        .saturating_add((code - 0xd800) << 10)
                                        .saturating_add(low - 0xdc00);
                                    position += 6;
                                    paired = true;
                                }
                            }
                            if !paired {
                                scalar = 0xfffd;
                            }
                        } else if (0xdc00..=0xdfff).contains(&code) {
                            scalar = 0xfffd;
                        }
                        output.push(char::from_u32(scalar).unwrap_or('\u{fffd}'));
                    }
                    _ => {
                        return Err(invalid_snapshot(
                            "Browser snapshot name has an unsupported escape",
                        ));
                    }
                }
            }
            character if character < '\u{20}' => {
                return Err(invalid_snapshot(
                    "Browser snapshot name contains a control character",
                ));
            }
            other => output.push(other),
        }
    }
    Err(invalid_snapshot("Browser snapshot name is unterminated"))
}

/// Split a YAML key from its optional inline text. The serializer emits
/// `role "name" [props...]`, optionally followed by `: text`. Only the props
/// region may contain `[ref=...]`; brackets and quotes in the name are never
/// searched for keys.
fn split_key_and_text(content: &str) -> Result<(String, &str), BrowserFailure> {
    if content.len() > MAX_ARIA_LINE_BYTES {
        return Err(invalid_snapshot("Browser snapshot line is too long"));
    }
    if let Some(rest) = content.strip_prefix('\'') {
        // YAML single-quoted key. A doubled apostrophe is one literal quote.
        let mut key = String::new();
        let mut chars = rest.char_indices();
        while let Some((index, character)) = chars.next() {
            if character == '\'' {
                if let Some((_, '\'')) = chars.clone().next() {
                    chars.next();
                    key.push('\'');
                    continue;
                }
                let remainder = &rest[index + 1..];
                if !remainder.is_empty() && !remainder.starts_with(':') {
                    return Err(invalid_snapshot(
                        "Browser snapshot contains an unrecognized line",
                    ));
                }
                return Ok((key, remainder));
            }
            key.push(character);
        }
        return Err(invalid_snapshot("Browser snapshot key is unterminated"));
    }
    let mut in_name = false;
    let mut escaped = false;
    let mut bracket_depth = 0usize;
    for (index, character) in content.char_indices() {
        if in_name {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_name = false;
            }
            continue;
        }
        match character {
            '"' if bracket_depth == 0 => in_name = true,
            '[' => bracket_depth = bracket_depth.saturating_add(1),
            ']' => {
                if bracket_depth == 0 {
                    return Err(invalid_snapshot(
                        "Browser snapshot contains an unbalanced property",
                    ));
                }
                bracket_depth -= 1;
            }
            ':' if bracket_depth == 0 => {
                let remainder = &content[index + 1..];
                if !remainder.is_empty() && !remainder.starts_with(' ') {
                    return Err(invalid_snapshot(
                        "Browser snapshot contains an unrecognized line",
                    ));
                }
                return Ok((content[..index].to_owned(), remainder));
            }
            _ => {}
        }
    }
    if in_name || bracket_depth != 0 {
        return Err(invalid_snapshot(
            "Browser snapshot contains an unterminated property",
        ));
    }
    Ok((content.to_owned(), ""))
}

fn valid_property_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Parse one `[key]` or `[key=value]` property and return its remainder.
fn parse_property(input: &str) -> Result<(&str, Option<&str>, &str), BrowserFailure> {
    let bytes = input.as_bytes();
    let mut index = 1usize;
    let mut quote = None;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(active) = quote {
            if byte == b'\\' {
                index = index.saturating_add(2);
                if index > bytes.len() {
                    return Err(invalid_snapshot(
                        "Browser snapshot property has an incomplete escape",
                    ));
                }
                continue;
            }
            if byte == active {
                quote = None;
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' | b'\'' => {
                quote = Some(byte);
                index += 1;
            }
            b'[' => {
                return Err(invalid_snapshot("Browser snapshot property is not flat"));
            }
            b']' => {
                let inner = &input[1..index];
                if inner.is_empty() || inner.bytes().any(|byte| byte.is_ascii_control()) {
                    return Err(invalid_snapshot(
                        "Browser snapshot contains an invalid property",
                    ));
                }
                let (name, value) = match inner.split_once('=') {
                    Some((name, value)) => (name, Some(value)),
                    None => (inner, None),
                };
                if !valid_property_name(name) {
                    return Err(invalid_snapshot(
                        "Browser snapshot contains an invalid property",
                    ));
                }
                return Ok((name, value, &input[index + 1..]));
            }
            _ => index += 1,
        }
    }
    Err(invalid_snapshot(
        "Browser snapshot property is unterminated",
    ))
}

#[derive(Debug)]
struct SnapshotLine {
    pub(super) role: String,
    pub(super) name: String,
    pub(super) reference: Option<String>,
    pub(super) reference_kind: Option<ElementReference>,
    pub(super) is_iframe: bool,
}

fn parse_element_line(key: &str) -> Result<SnapshotLine, BrowserFailure> {
    if key.is_empty() || key.len() > MAX_ARIA_LINE_BYTES {
        return Err(invalid_snapshot(
            "Browser snapshot contains an invalid element line",
        ));
    }
    let role_end = key.find([' ', '[', ':']).unwrap_or(key.len());
    let role = &key[..role_end];
    if role.is_empty()
        || role.len() > 64
        || !role
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(invalid_snapshot(
            "Browser snapshot contains an invalid element role",
        ));
    }
    let mut rest = key[role_end..].trim_start_matches(' ');
    let mut name = String::new();
    if rest.starts_with('"') {
        let (parsed, remainder) = parse_quoted(rest)?;
        name = parsed;
        rest = remainder;
        if !rest.is_empty() && !rest.starts_with([' ', '[', ':']) {
            return Err(invalid_snapshot(
                "Browser snapshot contains an unrecognized name suffix",
            ));
        }
    }
    let mut reference = None;
    let mut reference_kind = None;
    loop {
        rest = rest.trim_start_matches(' ');
        if rest.is_empty() || rest.starts_with(':') {
            break;
        }
        if !rest.starts_with('[') {
            return Err(invalid_snapshot(
                "Browser snapshot contains an unrecognized property",
            ));
        }
        let (property, value, remainder) = parse_property(rest)?;
        rest = remainder;
        if property != "ref" {
            continue;
        }
        if reference.is_some() {
            return Err(invalid_snapshot(
                "Browser snapshot contains duplicate element references",
            ));
        }
        let Some(value) = value else {
            return Err(invalid_snapshot(
                "Browser snapshot contains an empty element reference",
            ));
        };
        reference_kind = Some(parse_element_reference(value)?);
        reference = Some(value.to_owned());
    }
    Ok(SnapshotLine {
        role: role.to_owned(),
        name,
        reference,
        reference_kind,
        is_iframe: role == "iframe",
    })
}

fn validate_property_line(content: &str) -> Result<(), BrowserFailure> {
    let Some((name, _value)) = content[1..].split_once(':') else {
        return Err(invalid_snapshot(
            "Browser snapshot contains an unrecognized property line",
        ));
    };
    if !valid_property_name(name) {
        return Err(invalid_snapshot(
            "Browser snapshot contains an invalid property line",
        ));
    }
    Ok(())
}

pub(super) fn parse_elements(yaml: &str) -> Result<Vec<BrowserElement>, BrowserFailure> {
    let mut elements = Vec::new();
    let mut seen = BTreeSet::new();
    // (indent, frame label applying to deeper children)
    let mut scopes: Vec<(usize, Option<String>)> = Vec::new();
    let mut unnamed_frames = 0u64;
    for line in yaml.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len().saturating_sub(line.trim_start().len());
        while scopes
            .last()
            .is_some_and(|(child_indent, _)| *child_indent >= indent)
        {
            scopes.pop();
        }
        let Some(content) = line.trim_start().strip_prefix("- ") else {
            return Err(invalid_snapshot(
                "Browser snapshot contains an unrecognized line",
            ));
        };
        if content.starts_with('/') {
            validate_property_line(content)?;
            continue;
        }
        let (key, _text) = split_key_and_text(content)?;
        let parsed = parse_element_line(&key)?;
        // `- text: ...` is the serializer's text node, not an element.
        if parsed.role == "text" {
            continue;
        }
        let parent_frame = scopes.last().and_then(|(_, frame)| frame.clone());
        let (frame, child_frame) = if parsed.is_iframe {
            let child = match &parsed.reference_kind {
                Some(ElementReference::Frame(frame)) => frame.clone(),
                Some(ElementReference::Main) => {
                    format!("frame:{}", parsed.reference.as_deref().unwrap_or("unknown"))
                }
                None => {
                    unnamed_frames += 1;
                    format!("frame@{unnamed_frames}")
                }
            };
            // The iframe node itself is a child-frame boundary, so a credential
            // fill can never target it even at the top level.
            (
                parent_frame.clone().or_else(|| Some(child.clone())),
                Some(child),
            )
        } else {
            (parent_frame.clone(), parent_frame)
        };
        if let Some(reference) = parsed.reference {
            if !seen.insert(reference.clone()) {
                return Err(invalid_snapshot(
                    "Browser snapshot contains duplicate element references",
                ));
            }
            elements.push(BrowserElement {
                key: reference,
                role: parsed.role,
                name: parsed.name,
                frame,
            });
        }
        scopes.push((indent, child_frame));
    }
    Ok(elements)
}

pub(super) fn parse_action(text: &str, fallback_url: Option<&str>) -> BrowserAction {
    let (url, _) = page_fields(text);
    BrowserAction {
        url: url.or_else(|| fallback_url.map(str::to_owned)),
        detail: result_detail(text),
    }
}

pub(super) fn parse_snapshot(
    text: &str,
    result_truncated: bool,
) -> Result<BrowserSnapshot, BrowserFailure> {
    let (url, title) = page_fields(text);
    let url = url.ok_or_else(|| {
        BrowserFailure::new(
            "invalid_provider_result",
            "Browser worker snapshot did not include a page URL",
        )
    })?;
    let yaml = snapshot_yaml(text);
    let truncated = result_truncated || yaml.is_none();
    let body = yaml.unwrap_or_default();
    let elements = parse_elements(&body)?;
    Ok(BrowserSnapshot {
        url,
        title,
        elements,
        text: body,
        truncated,
    })
}
