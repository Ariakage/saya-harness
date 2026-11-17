use super::*;
use crate::global_memory::{MemoryCursor, MemoryReadOptions};
use serde_json::json;

impl ManagedMemoryTools {
    pub(super) fn execute_read(
        &self,
        args: args::ReadArgs,
        cancel: &AtomicBool,
    ) -> Result<ToolResult, BoundaryError> {
        if let Some(path) = &args.topic_path {
            return self.topic_page(&args, path, cancel);
        }
        let page = self.store.read_page_cancellable(
            MemoryReadOptions {
                entry_id: args.entry_id,
                fact_id: args.fact_id,
                cursor: args.cursor,
                max_bytes: args.max_bytes.unwrap_or(self.query_limits.max_page_bytes),
                max_items: args
                    .max_items
                    .unwrap_or(16.min(self.query_limits.max_results)),
            },
            self.query_limits,
            cancel,
        )?;
        if args
            .expected_revision
            .as_ref()
            .is_some_and(|expected| expected != &page.revision)
        {
            return Err(failure(
                "memory_conflict",
                "Memory read target changed since the requested revision",
            ));
        }
        self.query_result(
            serde_json::to_value(&page).map_err(|_| {
                failure(
                    "serialization_error",
                    "Memory read result cannot be encoded",
                )
            })?,
            page.truncated,
        )
    }
    fn topic_page(
        &self,
        args: &args::ReadArgs,
        path: &str,
        cancel: &AtomicBool,
    ) -> Result<ToolResult, BoundaryError> {
        let revision = args.expected_revision.as_ref().ok_or_else(|| {
            failure(
                "invalid_args",
                "A topic read requires its current memory revision",
            )
        })?;
        let query = digest(&encode(
            &(
                "saya-memory-topic-v1",
                self.store.root().to_string_lossy(),
                self.store.workspace(),
                path,
            ),
            self.limits.max_argument_bytes,
        )?);
        if let Some(cursor) = &args.cursor {
            if cursor.revision != *revision {
                return Err(failure(
                    "memory_cursor_stale",
                    "Memory topic cursor belongs to another snapshot",
                ));
            }
            if cursor.query_sha256 != query
                || cursor.record_index != 0
                || !valid_digest(&cursor.revision)
            {
                return Err(failure(
                    "memory_cursor_invalid",
                    "Memory topic cursor identity is invalid",
                ));
            }
        }
        let resource = self
            .store
            .read_resource_cancellable(path, Some(revision), cancel)
            .map_err(|error| {
                if args.cursor.is_some() && error.0.code == "memory_conflict" {
                    failure(
                        "memory_cursor_stale",
                        "Memory changed since this topic cursor was issued",
                    )
                } else {
                    error
                }
            })?;
        let start = args.cursor.as_ref().map_or(0, |cursor| cursor.byte_offset);
        if start > resource.text.len()
            || !resource.text.is_char_boundary(start)
            || (args.cursor.is_some() && start == resource.text.len())
        {
            return Err(failure(
                "memory_cursor_invalid",
                "Memory topic cursor position is invalid",
            ));
        }
        let max_bytes = args.max_bytes.unwrap_or(self.query_limits.max_page_bytes);
        let placeholder = MemoryCursor {
            revision: revision.clone(),
            query_sha256: query.clone(),
            record_index: 0,
            byte_offset: usize::MAX,
        };
        let mut value = json!({"snapshot_revision":revision,"visible_workspace":self.store.workspace(),"resource":resource.info,
            "start_byte":start,"end_byte":start,"total_bytes":resource.text.len(),"text":"","complete":false,"next_cursor":placeholder});
        let metadata = encode(&value, self.limits.max_result_bytes)?.len();
        if metadata >= max_bytes {
            return Err(failure(
                "memory_query_limit",
                "Memory topic metadata exceeds its page budget",
            ));
        }
        let mut bytes = 0usize;
        let mut end = start;
        for character in resource.text[start..].chars() {
            cancelled(cancel)?;
            let cost = match character {
                '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
                character if character <= '\u{1f}' => 6,
                character => character.len_utf8(),
            };
            if bytes.saturating_add(cost) > max_bytes - metadata {
                break;
            }
            bytes += cost;
            end += character.len_utf8();
        }
        if end == start && start < resource.text.len() {
            return Err(failure(
                "memory_query_limit",
                "Memory topic page cannot hold a UTF-8 character",
            ));
        }
        value["text"] = Value::String(resource.text[start..end].into());
        value["end_byte"] = end.into();
        let complete = start == 0 && end == resource.text.len();
        value["complete"] = complete.into();
        value["next_cursor"] = if end < resource.text.len() {
            serde_json::to_value(MemoryCursor {
                revision: revision.clone(),
                query_sha256: query,
                record_index: 0,
                byte_offset: end,
            })
            .map_err(|_| {
                failure(
                    "serialization_error",
                    "Memory topic cursor cannot be encoded",
                )
            })?
        } else {
            Value::Null
        };
        let _ = encode(&value, max_bytes)?;
        self.query_result(value, !complete)
    }
}
