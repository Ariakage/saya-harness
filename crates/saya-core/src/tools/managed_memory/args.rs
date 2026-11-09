use super::*;
use crate::global_memory::{MemoryCursor, MemoryLimits, MemorySearchOptions};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ModelScope {
    Global,
    Workspace,
}
impl ModelScope {
    pub(super) fn resolve(self, store: &GlobalMemoryStore) -> MemoryScope {
        match self {
            Self::Global => MemoryScope::Global,
            Self::Workspace => MemoryScope::Workspace {
                identity: store.workspace().id.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadArgs {
    pub(super) entry_id: Option<String>,
    pub(super) fact_id: Option<String>,
    pub(super) topic_path: Option<String>,
    pub(super) expected_revision: Option<String>,
    pub(super) cursor: Option<MemoryCursor>,
    pub(super) max_bytes: Option<usize>,
    pub(super) max_items: Option<usize>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SearchArgs {
    query: String,
    cursor: Option<MemoryCursor>,
    max_bytes: Option<usize>,
    max_items: Option<usize>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EvidenceArgs {
    pub(super) cursor: Option<MemoryEvidenceCursor>,
    pub(super) max_items: Option<usize>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FactArgs {
    pub(super) id: Option<String>,
    pub(super) kind: MemoryKind,
    pub(super) text: String,
    #[serde(default)]
    pub(super) aliases: Vec<String>,
    #[serde(default)]
    pub(super) derived_from: Vec<String>,
    pub(super) evidence: Vec<EvidenceSelector>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateArgs {
    pub(super) operation_id: String,
    pub(super) expected_revision: String,
    pub(super) scope: ModelScope,
    pub(super) entry_id: Option<String>,
    pub(super) title: Option<String>,
    pub(super) facts: Vec<FactArgs>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ForgetArgs {
    pub(super) operation_id: String,
    pub(super) expected_revision: String,
    pub(super) scope: ModelScope,
    pub(super) fact_ids: Vec<String>,
}
#[derive(Clone, Serialize)]
#[serde(tag = "tool", content = "arguments", rename_all = "snake_case")]
pub(super) enum Parsed {
    #[serde(rename = "memory_read")]
    Read(ReadArgs),
    #[serde(rename = "memory_search")]
    Search(SearchArgs),
    #[serde(rename = "memory_evidence")]
    Evidence(EvidenceArgs),
    #[serde(rename = "memory_update")]
    Update(UpdateArgs),
    #[serde(rename = "memory_forget")]
    Forget(ForgetArgs),
}
impl Parsed {
    pub(super) fn effect(&self) -> MemoryToolEffect {
        match self {
            Self::Read(_) | Self::Search(_) | Self::Evidence(_) => MemoryToolEffect::Read,
            _ => MemoryToolEffect::Mutation,
        }
    }
    pub(super) fn operation_id(&self) -> Option<&str> {
        match self {
            Self::Update(args) => Some(&args.operation_id),
            Self::Forget(args) => Some(&args.operation_id),
            _ => None,
        }
    }
    pub(super) fn expected_revision(&self) -> Option<&str> {
        match self {
            Self::Update(args) => Some(&args.expected_revision),
            Self::Forget(args) => Some(&args.expected_revision),
            Self::Read(args) => args
                .expected_revision
                .as_deref()
                .or_else(|| args.cursor.as_ref().map(|cursor| cursor.revision.as_str())),
            Self::Search(args) => args.cursor.as_ref().map(|cursor| cursor.revision.as_str()),
            Self::Evidence(_) => None,
        }
    }
}
fn invalid() -> BoundaryError {
    failure(
        "invalid_args",
        "Memory tool arguments are invalid or exceed their limits",
    )
}
fn page(
    max_bytes: Option<usize>,
    max_items: Option<usize>,
    queries: MemoryQueryLimits,
) -> Result<(), BoundaryError> {
    if max_bytes.is_some_and(|bytes| bytes == 0 || bytes > queries.max_page_bytes)
        || max_items.is_some_and(|items| items == 0 || items > queries.max_results)
    {
        return Err(invalid());
    }
    Ok(())
}
fn identity(operation_id: &str, revision: &str) -> Result<(), BoundaryError> {
    if !valid_id(operation_id, "op_") || !valid_digest(revision) {
        return Err(invalid());
    }
    Ok(())
}
pub(super) fn parse(
    call: &ToolCall,
    memory: MemoryLimits,
    queries: MemoryQueryLimits,
    limits: ManagedMemoryLimits,
) -> Result<Parsed, BoundaryError> {
    if call.arguments.len() > limits.max_argument_bytes
        || call.call_id.is_empty()
        || call.call_id.len() > 256
        || call.call_id.chars().any(char::is_control)
    {
        return Err(invalid());
    }
    let parsed = match call.name.as_str() {
        "memory_read" => {
            let args: ReadArgs = serde_json::from_str(&call.arguments).map_err(|_| invalid())?;
            page(args.max_bytes, args.max_items, queries)?;
            if args
                .entry_id
                .as_ref()
                .is_some_and(|id| !valid_id(id, "mem_"))
                || args
                    .fact_id
                    .as_ref()
                    .is_some_and(|id| !valid_id(id, "fact_"))
                || args
                    .expected_revision
                    .as_ref()
                    .is_some_and(|revision| !valid_digest(revision))
            {
                return Err(invalid());
            }
            if let Some(path) = &args.topic_path
                && (args.entry_id.is_some()
                    || args.fact_id.is_some()
                    || args.expected_revision.is_none()
                    || path.len() > 512
                    || !path.starts_with("memory/")
                    || !path.ends_with(".md")
                    || path.contains('\\')
                    || path.chars().any(char::is_control)
                    || path
                        .split('/')
                        .any(|part| part.is_empty() || part.starts_with('.')))
            {
                return Err(invalid());
            }
            Parsed::Read(args)
        }
        "memory_search" => {
            let args: SearchArgs = serde_json::from_str(&call.arguments).map_err(|_| invalid())?;
            page(args.max_bytes, args.max_items, queries)?;
            if args.query.trim().is_empty() || args.query.len() > queries.max_query_bytes {
                return Err(invalid());
            }
            Parsed::Search(args)
        }
        "memory_evidence" => {
            let args: EvidenceArgs =
                serde_json::from_str(&call.arguments).map_err(|_| invalid())?;
            if args.max_items.is_some_and(|items| items == 0 || items > 32)
                || args.cursor.as_ref().is_some_and(|cursor| {
                    !valid_digest(&cursor.history_sha256)
                        || cursor.snapshot_items > 65_536
                        || cursor.before_item > cursor.snapshot_items
                })
            {
                return Err(invalid());
            }
            Parsed::Evidence(args)
        }
        "memory_update" => {
            let args: UpdateArgs = serde_json::from_str(&call.arguments).map_err(|_| invalid())?;
            identity(&args.operation_id, &args.expected_revision)?;
            if args
                .entry_id
                .as_ref()
                .is_some_and(|id| !valid_id(id, "mem_"))
                || args.facts.len() > limits.max_changed_facts
                || args.title.as_ref().is_some_and(|title| {
                    title.trim().is_empty()
                        || title.len() > 256
                        || title.chars().any(char::is_control)
                })
                || (args.entry_id.is_none() && (args.title.is_none() || args.facts.is_empty()))
            {
                return Err(invalid());
            }
            let mut ids = std::collections::BTreeSet::new();
            for fact in &args.facts {
                if fact
                    .id
                    .as_ref()
                    .is_some_and(|id| !valid_id(id, "fact_") || !ids.insert(id))
                    || fact.text.trim().is_empty()
                    || fact.text.len() > memory.max_fact_bytes
                    || fact.aliases.len() > 32
                    || fact.aliases.iter().any(|alias| {
                        alias.trim().is_empty()
                            || alias.len() > 256
                            || alias.chars().any(char::is_control)
                    })
                    || fact.derived_from.len() > 32
                    || fact.derived_from.iter().any(|id| !valid_id(id, "fact_"))
                    || fact.evidence.is_empty()
                    || fact.evidence.len() > limits.max_evidence_per_fact
                {
                    return Err(invalid());
                }
            }
            Parsed::Update(args)
        }
        "memory_forget" => {
            let args: ForgetArgs = serde_json::from_str(&call.arguments).map_err(|_| invalid())?;
            identity(&args.operation_id, &args.expected_revision)?;
            if args.fact_ids.is_empty()
                || args.fact_ids.len() > limits.max_changed_facts
                || args.fact_ids.iter().any(|id| !valid_id(id, "fact_"))
            {
                return Err(invalid());
            }
            Parsed::Forget(args)
        }
        _ => return Err(failure("unknown_tool", "Memory tool name is unknown")),
    };
    Ok(parsed)
}
impl SearchArgs {
    pub(super) fn options(&self, limits: MemoryQueryLimits) -> MemorySearchOptions {
        MemorySearchOptions {
            query: self.query.clone(),
            cursor: self.cursor.clone(),
            max_bytes: self.max_bytes.unwrap_or(limits.max_page_bytes),
            max_items: self.max_items.unwrap_or(16.min(limits.max_results)),
        }
    }
}

pub(super) fn definitions(
    memory: MemoryLimits,
    queries: MemoryQueryLimits,
    limits: ManagedMemoryLimits,
) -> Vec<ToolDefinition> {
    let cursor = json!({"type":"object","properties":{
        "revision":{"type":"string","pattern":"^[0-9a-f]{64}$"}, "query_sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"},
        "record_index":{"type":"integer","minimum":0}, "byte_offset":{"type":"integer","minimum":0}},
        "required":["revision","query_sha256","record_index","byte_offset"],"additionalProperties":false});
    let id = |prefix: &str| json!({"type":"string","minLength":prefix.len()+1,"maxLength":128,"pattern":format!("^{prefix}[A-Za-z0-9_-]+$")});
    let revision = json!({"type":"string","pattern":"^[0-9a-f]{64}$"});
    let scope = json!({"type":"string","enum":["global","workspace"]});
    let evidence = json!({"oneOf":[
        {"type":"object","properties":{"kind":{"const":"user_message"},"ordinal":{"type":"integer","minimum":1}},"required":["kind","ordinal"],"additionalProperties":false},
        {"type":"object","properties":{"kind":{"const":"tool_result"},"call_id":{"type":"string","minLength":1,"maxLength":256}},"required":["kind","call_id"],"additionalProperties":false}]});
    let fact = json!({"type":"object","properties":{
        "id":id("fact_"),"kind":{"type":"string","enum":["explicit_preference","user_correction","observed_fact","task_decision","inference"]},
        "text":{"type":"string","minLength":1,"maxLength":memory.max_fact_bytes},
        "aliases":{"type":"array","maxItems":32,"items":{"type":"string","minLength":1,"maxLength":256}},
        "derived_from":{"type":"array","maxItems":32,"items":id("fact_")},
        "evidence":{"type":"array","minItems":1,"maxItems":limits.max_evidence_per_fact,"items":evidence}},
        "required":["kind","text","evidence"],"additionalProperties":false});
    let page_bytes = json!({"type":"integer","minimum":1,"maximum":queries.max_page_bytes});
    let page_items = json!({"type":"integer","minimum":1,"maximum":queries.max_results});
    let schemas = [
        (
            "memory_read",
            "Read current visible memory facts or a topic path supplied by the memory index. Topic reads require the current snapshot revision. Check fragment completeness and continue with the returned cursor.",
            json!({"type":"object","properties":{"entry_id":id("mem_"),"fact_id":id("fact_"),"topic_path":{"type":"string","minLength":1,"maxLength":512},"expected_revision":revision,
                "cursor":cursor,"max_bytes":page_bytes,"max_items":page_items},"additionalProperties":false}),
        ),
        (
            "memory_search",
            "Search current scoped memory text and aliases with a bounded literal query. Source references remain opaque evidence identities.",
            json!({"type":"object","properties":{"query":{"type":"string","minLength":1,"maxLength":queries.max_query_bytes},"cursor":cursor,"max_bytes":page_bytes,"max_items":page_items},"required":["query"],"additionalProperties":false}),
        ),
        (
            "memory_evidence",
            "Discover actual current-session source selectors, hashes, and observed states before memory_update. Defaults to the newest 32 eligible sources. User-message ordinal is the original persisted 1-based UserText ordinal and remains stable through context compaction. Follow next_cursor for older sources. Memory tool outputs are excluded as independent facts. Cursor fields identify a fixed append-only historical snapshot and cannot select a different session or root.",
            json!({"type":"object","properties":{"max_items":{"type":"integer","minimum":1,"maximum":32,"default":32},"cursor":{"type":"object","properties":{"session_id":{"type":"string","format":"uuid"},"history_sha256":revision,"snapshot_items":{"type":"integer","minimum":0,"maximum":65536},"before_item":{"type":"integer","minimum":0,"maximum":65536}},"required":["session_id","history_sha256","snapshot_items","before_item"],"additionalProperties":false}},"additionalProperties":false}),
        ),
        (
            "memory_update",
            "Add or update memory facts using current-session evidence selectors returned by memory_evidence. Existing facts omitted from the request remain. Use stable IDs for corrections and memory_forget for deletion. A global fact requires user-message support. Memory tool outputs cannot provide independent fact evidence. A proposal is saved only after a successful recorded result.",
            json!({"type":"object","properties":{"operation_id":id("op_"),"expected_revision":revision,"scope":scope,"entry_id":id("mem_"),"title":{"type":"string","minLength":1,"maxLength":256},
                "facts":{"type":"array","maxItems":limits.max_changed_facts,"items":fact}},"required":["operation_id","expected_revision","scope","facts"],"additionalProperties":false}),
        ),
        (
            "memory_forget",
            "Forget explicit current fact IDs and their declared aliases and derivations within the selected scope. The result reports active-memory coverage and retained history. Preserve unknown outcomes for host recovery.",
            json!({"type":"object","properties":{"operation_id":id("op_"),"expected_revision":revision,"scope":scope,"fact_ids":{"type":"array","minItems":1,"maxItems":limits.max_changed_facts,"items":id("fact_")}},
                "required":["operation_id","expected_revision","scope","fact_ids"],"additionalProperties":false}),
        ),
    ];
    schemas
        .into_iter()
        .map(|(name, description, input_schema)| ToolDefinition {
            name: name.into(),
            description: description.into(),
            input_schema,
        })
        .collect()
}
