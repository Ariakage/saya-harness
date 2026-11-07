//! Versioned, bounded projections of durable session snapshots.
use crate::{
    contract::{BoundaryError, ErrorInfo, HistoryItem},
    sqlite::SessionSnapshot,
};
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportScope {
    #[default]
    Transcript,
    Full,
}
#[derive(Debug, Clone, Copy)]
pub struct ExportOptions {
    pub scope: ExportScope,
    pub include_tools: bool,
    pub max_field_bytes: usize,
    pub max_output_bytes: usize,
}
impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            scope: ExportScope::Transcript,
            include_tools: false,
            max_field_bytes: 65_536,
            max_output_bytes: 32 * 1024 * 1024,
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct ExportMetadata {
    pub created_at: String,
    pub source_database_schema: i64,
    pub last_change_seq: u64,
    pub last_event_seq: u64,
    pub scope: ExportScope,
    pub include_tools: bool,
    pub redaction_scope: &'static str,
    pub max_field_bytes: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct TruncatedField {
    pub path: String,
    pub original_bytes: usize,
    pub retained_bytes: usize,
}
/// Public export format v1, independent of runtime and database schema versions.
#[derive(Debug, Clone, Serialize)]
pub struct SessionExport {
    pub schema_version: u8,
    pub export_type: &'static str,
    pub snapshot: ExportMetadata,
    pub session: Value,
    pub history: Vec<Value>,
    pub runs: Vec<Value>,
    pub tool_calls: Vec<Value>,
    pub metrics: Vec<Value>,
    pub background_jobs: Vec<Value>,
    pub events: Vec<Value>,
    pub activities: Vec<Value>,
    pub project_context: Vec<Value>,
    pub approval_requests: Vec<Value>,
    pub omitted_fields: Vec<String>,
    pub truncated_fields: Vec<TruncatedField>,
    #[serde(skip)]
    max_output_bytes: usize,
}
fn error(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}
fn result_summary(result: &Value) -> Value {
    if result.is_null() {
        return Value::Null;
    }
    json!({"ok":result["ok"],"truncated":result["truncated"],"error_code":result["error"]["code"],"body_omitted":true})
}
fn selected_call(call: &Value, include: bool) -> Value {
    if include {
        return call.clone();
    }
    json!({"call_id":call["call_id"],"name":call["name"],"arguments_omitted":true})
}
fn selected_event(event: &Value, include: bool) -> Value {
    let mut value = event.clone();
    if include {
        return value;
    }
    match value["type"].as_str() {
        Some("tool.requested") => {
            if let Some(data) = value["data"].as_object_mut() {
                data.remove("args");
                data.insert("arguments_omitted".into(), true.into());
            }
        }
        Some("tool.finished") => {
            value["data"]["result"] = result_summary(&value["data"]["result"]);
        }
        Some("permission.requested") => {
            if let Some(data) = value["data"].as_object_mut() {
                data.remove("operation_summary");
            }
        }
        _ => {}
    }
    value
}
fn history(snapshot: &SessionSnapshot, include: bool) -> Result<Vec<Value>, BoundaryError> {
    let mut result = Vec::new();
    let mut associations = std::collections::HashMap::new();
    let mut persisted = std::collections::HashMap::new();
    for call in &snapshot.tool_calls {
        persisted
            .entry(call["call"].to_string())
            .and_modify(|run| *run = Value::Null)
            .or_insert_with(|| call["run_id"].clone());
    }
    for (ordinal, item) in snapshot.session.history.iter().enumerate() {
        let value = match item {
            HistoryItem::UserText(text) => {
                json!({"ordinal":ordinal,"kind":"user_text","text":text})
            }
            HistoryItem::AssistantText(text) => {
                json!({"ordinal":ordinal,"kind":"assistant_text","text":text})
            }
            HistoryItem::AssistantPartial(text) => {
                json!({"ordinal":ordinal,"kind":"assistant_partial","text":text,"partial":true})
            }
            HistoryItem::ToolCall(call) => {
                let raw = serde_json::to_value(call)
                    .map_err(|_| error("export_error", "cannot encode tool call"))?;
                let run = persisted
                    .get(&raw.to_string())
                    .cloned()
                    .unwrap_or(Value::Null);
                associations.insert(call.call_id.clone(), run.clone());
                json!({"ordinal":ordinal,"kind":"tool_call","run_id":run,"call_id":call.call_id,"call":selected_call(&raw,include)})
            }
            HistoryItem::ToolResult { call_id, result } => {
                let raw = serde_json::to_value(result)
                    .map_err(|_| error("export_error", "cannot encode tool result"))?;
                json!({"ordinal":ordinal,"kind":"tool_result","run_id":associations.get(call_id),"call_id":call_id,"result":if include {raw}else{result_summary(&raw)}})
            }
            HistoryItem::ProviderItem { .. } => continue,
        };
        result.push(value);
    }
    Ok(result)
}
fn truncate(
    value: &mut Value,
    path: &str,
    max: usize,
    fields: &mut Vec<TruncatedField>,
    depth: usize,
) -> Result<(), BoundaryError> {
    if depth > 128 {
        return Err(error("export_limit", "export nesting limit exceeded"));
    }
    match value {
        Value::String(text) if text.len() > max => {
            if matches!(
                path.rsplit('/').next(),
                Some("id" | "run_id" | "call_id" | "job_id" | "parent_call_id")
            ) {
                return Err(error(
                    "export_limit",
                    "an identity exceeds the export field limit",
                ));
            }
            let original_bytes = text.len();
            let mut end = max;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            fields.push(TruncatedField {
                path: path.into(),
                original_bytes,
                retained_bytes: end,
            });
        }
        Value::Array(values) => {
            for (i, value) in values.iter_mut().enumerate() {
                truncate(value, &format!("{path}/{i}"), max, fields, depth + 1)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values.iter_mut() {
                let key = key.replace('~', "~0").replace('/', "~1");
                truncate(value, &format!("{path}/{key}"), max, fields, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}
impl SessionExport {
    /// Project one immutable snapshot. Redaction precedes truncation and applies
    /// to all selected content and object keys. Provider-owned items are omitted.
    /// This function performs no I/O, model calls, or source-session mutation.
    pub fn from_snapshot(
        snapshot: &SessionSnapshot,
        options: ExportOptions,
        secrets: &[String],
    ) -> Result<Self, BoundaryError> {
        if options.max_field_bytes == 0 || options.max_output_bytes == 0 {
            return Err(error("invalid_argument", "export limits must be positive"));
        }
        let full = options.scope == ExportScope::Full;
        let mut session = serde_json::to_value(&snapshot.session)
            .map_err(|_| error("export_error", "cannot encode session metadata"))?;
        if let Some(fields) = session.as_object_mut() {
            fields.remove("history");
        }
        let tool_calls=snapshot.tool_calls.iter().map(|stored|json!({"run_id":stored["run_id"],"call_id":stored["call_id"],"state":stored["state"],"call":selected_call(&stored["call"],options.include_tools),"result":if options.include_tools {stored["result"].clone()}else{result_summary(&stored["result"])}})).collect::<Vec<_>>();
        let background_jobs = snapshot
            .background_jobs
            .iter()
            .map(|job| {
                let mut job = job.clone();
                if !options.include_tools {
                    job["result"] = result_summary(&job["result"]);
                }
                job
            })
            .collect::<Vec<_>>();
        let mut approvals = Vec::new();
        if full {
            for approval in &snapshot.approval_requests {
                let mut approval = approval.clone();
                if !options.include_tools
                    && let Some(fields) = approval["request"].as_object_mut()
                {
                    fields.remove("arguments");
                    fields.remove("operation_summary");
                    fields.insert("arguments_omitted".into(), true.into());
                }
                approvals.push(approval);
            }
        }
        let mut activities = Vec::new();
        if full {
            for change in &snapshot.changes {
                if change["kind"] == "activity" {
                    let raw = change["reference"]
                        .as_str()
                        .ok_or_else(|| error("storage_corrupt", "missing activity record"))?;
                    let event: Value = serde_json::from_str(raw)
                        .map_err(|_| error("storage_corrupt", "invalid activity record"))?;
                    activities.push(json!({"seq":change["seq"],"run_id":change["run_id"],"event":selected_event(&event,options.include_tools)}));
                }
            }
        }
        let mut omitted_fields = vec!["provider_internal_state".into()];
        if !full {
            omitted_fields.extend(
                ["events", "activities", "metrics", "approval_requests"].map(str::to_owned),
            );
        }
        if !options.include_tools {
            omitted_fields.push("tool_arguments_and_result_bodies".into());
        }
        let history = history(snapshot, options.include_tools)?;
        if history.iter().any(|item| {
            matches!(item["kind"].as_str(), Some("tool_call" | "tool_result"))
                && item["run_id"].is_null()
        }) {
            omitted_fields.push("unavailable_history_run_association".into());
        }
        let raw = json!({"session":session,"history":history,"runs":snapshot.runs,"tool_calls":tool_calls,"background_jobs":background_jobs,
            "metrics":if full{snapshot.metrics.clone()}else{vec![]},"events":if full{snapshot.events.iter().map(|e|selected_event(e,options.include_tools)).collect::<Vec<_>>()}else{vec![]},
            "activities":activities,"project_context":snapshot.project_context,"approval_requests":approvals});
        let mut selected = raw;
        crate::redaction::redact(&mut selected, secrets)?;
        let mut truncated_fields = Vec::new();
        truncate(
            &mut selected,
            "",
            options.max_field_bytes,
            &mut truncated_fields,
            0,
        )?;
        // Deserialize only the projected collections, never opaque provider state.
        let collection = |name: &str| -> Result<Vec<Value>, BoundaryError> {
            selected[name]
                .as_array()
                .cloned()
                .ok_or_else(|| error("export_error", "invalid projected collection"))
        };
        let mut metadata = json!({"created_at":snapshot.created_at});
        crate::redaction::redact(&mut metadata, secrets)?;
        let value = Self {
            schema_version: 1,
            export_type: "saya.session",
            snapshot: ExportMetadata {
                created_at: metadata["created_at"]
                    .as_str()
                    .ok_or_else(|| error("export_error", "invalid export timestamp"))?
                    .into(),
                source_database_schema: snapshot.source_database_schema,
                last_change_seq: snapshot.last_change_seq,
                last_event_seq: snapshot.last_event_seq,
                scope: options.scope,
                include_tools: options.include_tools,
                redaction_scope: "persisted-markers-and-current-configured-values",
                max_field_bytes: options.max_field_bytes,
            },
            session: selected["session"].clone(),
            history: collection("history")?,
            runs: collection("runs")?,
            tool_calls: collection("tool_calls")?,
            metrics: collection("metrics")?,
            background_jobs: collection("background_jobs")?,
            events: collection("events")?,
            activities: collection("activities")?,
            project_context: collection("project_context")?,
            approval_requests: collection("approval_requests")?,
            omitted_fields,
            truncated_fields,
            max_output_bytes: options.max_output_bytes,
        };
        value.to_json()?;
        Ok(value)
    }
    /// Encode export schema v1, refusing an output that exceeds the total limit.
    pub fn to_json(&self) -> Result<Vec<u8>, BoundaryError> {
        let mut bytes = serde_json::to_vec_pretty(self)
            .map_err(|_| error("export_error", "cannot encode session export"))?;
        bytes.push(b'\n');
        if bytes.len() > self.max_output_bytes {
            return Err(error(
                "export_limit",
                "encoded session export exceeds output limit",
            ));
        }
        Ok(bytes)
    }
}
