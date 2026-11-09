use super::*;

fn cursor(
    payload: &Payload,
    requested: Option<OutputCursor>,
    cancel: &AtomicBool,
) -> Result<OutputCursor, BoundaryError> {
    let view = payload.digest_cancellable(cancel)?;
    let current = requested.unwrap_or_else(|| OutputCursor {
        segment: 0,
        byte_offset: 0,
        view_sha256: view.clone(),
    });
    if current.view_sha256 != view {
        return Err(error(
            "cursor_invalid",
            "Output cursor belongs to another or changed text projection",
        ));
    }
    if current.segment > payload.segments.len()
        || (current.segment == payload.segments.len() && current.byte_offset != 0)
        || payload
            .segments
            .get(current.segment)
            .is_some_and(|segment| {
                current.byte_offset > segment.text.len()
                    || !segment.text.is_char_boundary(current.byte_offset)
            })
    {
        return Err(error(
            "cursor_invalid",
            "Output cursor is outside the retained text",
        ));
    }
    Ok(current)
}
fn normalize(payload: &Payload, mut cursor: OutputCursor) -> Option<OutputCursor> {
    while let Some(segment) = payload.segments.get(cursor.segment) {
        if cursor.byte_offset < segment.text.len() {
            return Some(cursor);
        }
        cursor.segment += 1;
        cursor.byte_offset = 0;
    }
    None
}

pub(super) fn read(
    payload: Payload,
    digest: &str,
    options: ReadOptions,
    limits: OutputLimits,
    cancel: &AtomicBool,
) -> Result<OutputPage, BoundaryError> {
    if options.max_bytes == 0
        || options.max_bytes > limits.max_page_bytes
        || options.max_lines == 0
        || options.max_lines > limits.max_page_lines
    {
        return Err(error("invalid_arguments", "Output page limits are invalid"));
    }
    let mut next = normalize(&payload, cursor(&payload, options.cursor, cancel)?);
    let mut remaining_bytes = options.max_bytes;
    let mut remaining_lines = options.max_lines;
    let mut chunks = Vec::new();
    while let Some(mut position) = next.take() {
        check_cancelled(cancel)?;
        let segment = &payload.segments[position.segment];
        let source = &segment.text[position.byte_offset..];
        let candidate = prefix(source, remaining_bytes);
        let mut lines = 0;
        let mut end = candidate.len();
        for (offset, byte) in candidate.bytes().enumerate() {
            if byte == b'\n' {
                lines += 1;
                if lines == remaining_lines {
                    end = offset + 1;
                    break;
                }
            }
        }
        let text = &candidate[..end];
        if text.is_empty() {
            if chunks.is_empty() {
                return Err(error(
                    "output_query_limit",
                    "Output page cannot fit the next UTF-8 character",
                ));
            }
            next = Some(position);
            break;
        }
        let used_lines =
            text.bytes().filter(|byte| *byte == b'\n').count() + usize::from(!text.ends_with('\n'));
        chunks.push(OutputChunk {
            source_window: segment.source_window,
            segment: position.segment,
            byte_offset: position.byte_offset,
            text: text.into(),
        });
        position.byte_offset += text.len();
        remaining_bytes -= text.len();
        remaining_lines = remaining_lines.saturating_sub(used_lines);
        next = normalize(&payload, position);
        if remaining_bytes == 0 || remaining_lines == 0 {
            break;
        }
    }
    check_cancelled(cancel)?;
    Ok(OutputPage {
        metadata: payload.metadata(digest, limits.preview_bytes),
        chunks,
        next_cursor: next,
    })
}

pub(super) fn search(
    payload: Payload,
    digest: &str,
    options: SearchOptions,
    limits: OutputLimits,
    cancel: &AtomicBool,
) -> Result<OutputSearchPage, BoundaryError> {
    if options.pattern.is_empty()
        || options.pattern.len() > 4096
        || options.max_matches == 0
        || options.max_matches > limits.max_search_matches
        || options.context_lines > 20
        || options.max_bytes == 0
        || options.max_bytes > limits.max_page_bytes
    {
        return Err(error(
            "invalid_arguments",
            "Output search limits are invalid",
        ));
    }
    let matcher = if options.regex {
        Some(
            regex::RegexBuilder::new(&options.pattern)
                .size_limit(1024 * 1024)
                .dfa_size_limit(1024 * 1024)
                .build()
                .map_err(|_| {
                    error(
                        "invalid_arguments",
                        "Output search expression is invalid or too large",
                    )
                })?,
        )
    } else {
        None
    };
    let requested = cursor(&payload, options.cursor, cancel)?;
    let mut matches = Vec::new();
    let mut remaining = options.max_bytes;
    let mut next = None;
    'segments: for segment_index in requested.segment..payload.segments.len() {
        check_cancelled(cancel)?;
        let segment = &payload.segments[segment_index];
        let start = if segment_index == requested.segment {
            requested.byte_offset
        } else {
            0
        };
        let mut offset = start;
        let initial_line = segment.text[..start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1;
        for (line_number, line) in (initial_line..).zip(segment.text[start..].split_inclusive('\n'))
        {
            check_cancelled(cancel)?;
            // regex is line-oriented and intentionally bounded. Literal search
            // can still locate a match in an arbitrarily long retained line.
            if matcher.is_some() && line.len() > 64 * 1024 {
                return Err(error(
                    "output_query_limit",
                    "Regex search requires lines at most 64 KiB; use literal search or pages",
                ));
            }
            let found = match &matcher {
                Some(matcher) => matcher
                    .find(line)
                    .map(|found| (found.start(), found.start() == found.end())),
                None => line.find(&options.pattern).map(|offset| (offset, false)),
            };
            check_cancelled(cancel)?;
            if let Some((found, empty_match)) = found {
                let match_offset = offset + found;
                let required_end = match_offset
                    + if empty_match {
                        0
                    } else {
                        segment.text[match_offset..]
                            .chars()
                            .next()
                            .map_or(0, char::len_utf8)
                    };
                let (window_start, window_end) = context_window(
                    &segment.text,
                    offset,
                    offset + line.len(),
                    options.context_lines,
                );
                let preceding = (remaining / 3).max(usize::from(empty_match));
                let mut excerpt_start = match_offset.saturating_sub(preceding).max(window_start);
                while !segment.text.is_char_boundary(excerpt_start) {
                    excerpt_start -= 1;
                }
                let excerpt = prefix(&segment.text[excerpt_start..window_end], remaining);
                if excerpt.is_empty() || excerpt_start + excerpt.len() < required_end {
                    if matches.is_empty() {
                        return Err(error(
                            "output_query_limit",
                            "Output search page cannot fit the matched character",
                        ));
                    }
                    next = Some(OutputCursor {
                        segment: segment_index,
                        byte_offset: offset,
                        view_sha256: requested.view_sha256.clone(),
                    });
                    break 'segments;
                }
                matches.push(SearchHit {
                    source_window: segment.source_window,
                    segment: segment_index,
                    line: line_number,
                    byte_offset: excerpt_start,
                    text: excerpt.into(),
                    truncated: excerpt_start > window_start
                        || excerpt_start + excerpt.len() < window_end,
                    cursor: OutputCursor {
                        segment: segment_index,
                        byte_offset: excerpt_start,
                        view_sha256: requested.view_sha256.clone(),
                    },
                });
                remaining -= excerpt.len();
                if matches.len() == options.max_matches || remaining == 0 {
                    next = normalize(
                        &payload,
                        OutputCursor {
                            segment: segment_index,
                            byte_offset: offset + line.len(),
                            view_sha256: requested.view_sha256.clone(),
                        },
                    );
                    break 'segments;
                }
            }
            offset += line.len();
        }
    }
    check_cancelled(cancel)?;
    Ok(OutputSearchPage {
        metadata: payload.metadata(digest, limits.preview_bytes),
        matches,
        next_cursor: next,
    })
}

fn context_window(
    text: &str,
    line_start: usize,
    line_end: usize,
    context_lines: usize,
) -> (usize, usize) {
    let mut start = line_start;
    let mut end = line_end;
    for _ in 0..context_lines {
        if start > 0 {
            let before = text[..start].strip_suffix('\n').unwrap_or(&text[..start]);
            start = before.rfind('\n').map_or(0, |newline| newline + 1);
        }
        if end < text.len() {
            end += text[end..]
                .find('\n')
                .map_or(text.len() - end, |newline| newline + 1);
        }
    }
    (start, end)
}
