use super::*;
use crate::{
    contract::HistoryItem,
    global_memory::{MemorySource, MemorySourceKind},
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EvidenceSelector {
    UserMessage { ordinal: usize },
    ToolResult { call_id: String },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    UserInput,
    Succeeded,
    Failed,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEvidence {
    pub selector: EvidenceSelector,
    pub origin_scope: MemoryScope,
    pub source_reference: String,
    pub revision: String,
    pub state: EvidenceState,
    pub result_truncated: bool,
    pub excerpt: Value,
    pub excerpt_truncated: bool,
}
pub(super) struct RedactionProbe {
    raw: Value,
    redacted_digest: String,
}
impl RedactionProbe {
    pub(super) fn check(&self, tools: &ManagedMemoryTools) -> Result<(), BoundaryError> {
        let current = tools.store.redact_review_value(self.raw.clone())?;
        if digest(&encode(&current, tools.limits.max_history_bytes)?) != self.redacted_digest {
            return Err(failure(
                "stale_approval",
                "Memory review credential projection changed",
            ));
        }
        Ok(())
    }
}
pub(super) fn probe(
    tools: &ManagedMemoryTools,
    raw: Value,
) -> Result<(Value, RedactionProbe), BoundaryError> {
    let safe = tools.store.redact_review_value(raw.clone())?;
    let redacted_digest = digest(&encode(&safe, tools.limits.max_history_bytes)?);
    Ok((
        safe,
        RedactionProbe {
            raw,
            redacted_digest,
        },
    ))
}
struct Resolved {
    info: MemoryEvidence,
    probe: RedactionProbe,
}
#[derive(Debug, Clone, Serialize)]
pub struct MemoryEvidenceCatalogItem {
    pub selector: EvidenceSelector,
    pub revision: String,
    pub state: EvidenceState,
    pub result_truncated: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct MemoryEvidenceCatalog {
    pub session_id: SessionId,
    pub workspace_identity: String,
    pub entries: Vec<MemoryEvidenceCatalogItem>,
    pub truncated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryEvidenceCursor {
    pub session_id: SessionId,
    pub history_sha256: String,
    pub snapshot_items: usize,
    pub before_item: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct MemoryEvidencePage {
    pub session_id: SessionId,
    pub workspace_identity: String,
    pub history_sha256: String,
    pub snapshot_items: usize,
    pub entries: Vec<MemoryEvidenceCatalogItem>,
    pub excluded: usize,
    pub truncated: bool,
    pub next_cursor: Option<MemoryEvidenceCursor>,
}

fn invalid() -> BoundaryError {
    failure(
        "memory_evidence_invalid",
        "Memory evidence is missing, ambiguous or differs from its persisted history type",
    )
}
pub(super) fn hash<T: Serialize + ?Sized>(
    value: &T,
    max: usize,
    cancel: &AtomicBool,
) -> Result<String, BoundaryError> {
    struct HashWriter<'a> {
        hash: Sha256,
        bytes: usize,
        max: usize,
        cancel: &'a AtomicBool,
    }
    impl Write for HashWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.cancel.load(Ordering::Acquire)
                || self.bytes.saturating_add(bytes.len()) > self.max
            {
                return Err(std::io::Error::other("memory evidence limit"));
            }
            self.bytes += bytes.len();
            self.hash.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    cancelled(cancel)?;
    let mut writer = HashWriter {
        hash: Sha256::new(),
        bytes: 0,
        max,
        cancel,
    };
    if serde_json::to_writer(&mut writer, value).is_err() {
        cancelled(cancel)?;
        return Err(failure(
            "memory_evidence_limit",
            "Persisted memory evidence exceeds its encoded byte limit",
        ));
    }
    Ok(writer
        .hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
fn clip(text: &str, max: usize) -> (String, bool) {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].into(), end < text.len())
}

struct Index<'a> {
    session: &'a Session,
    users: Vec<usize>,
    calls: BTreeMap<&'a str, Vec<usize>>,
    results: BTreeMap<&'a str, Vec<usize>>,
}
impl<'a> Index<'a> {
    fn new(session: &'a Session, cancel: &AtomicBool) -> Result<Self, BoundaryError> {
        if session.history.len() > 65_536 {
            return Err(failure(
                "memory_evidence_limit",
                "Session history exceeds the memory evidence item limit",
            ));
        }
        let mut index = Self {
            session,
            users: Vec::new(),
            calls: BTreeMap::new(),
            results: BTreeMap::new(),
        };
        for (position, item) in session.history.iter().enumerate() {
            cancelled(cancel)?;
            match item {
                HistoryItem::UserText(_) => index.users.push(position),
                HistoryItem::ToolCall(call) => {
                    index.calls.entry(&call.call_id).or_default().push(position)
                }
                HistoryItem::ToolResult { call_id, .. } => {
                    index.results.entry(call_id).or_default().push(position)
                }
                _ => {}
            }
        }
        Ok(index)
    }
    fn resolve(
        &self,
        tools: &ManagedMemoryTools,
        selector: &EvidenceSelector,
        cancel: &AtomicBool,
    ) -> Result<Resolved, BoundaryError> {
        let origin_scope = MemoryScope::Workspace {
            identity: tools.store.workspace().id.clone(),
        };
        match selector {
            EvidenceSelector::UserMessage { ordinal } => {
                let position = ordinal
                    .checked_sub(1)
                    .and_then(|ordinal| self.users.get(ordinal))
                    .copied()
                    .ok_or_else(invalid)?;
                let item = &self.session.history[position];
                let HistoryItem::UserText(text) = item else {
                    return Err(invalid());
                };
                if text.trim().is_empty() {
                    return Err(invalid());
                }
                let revision = hash(item, tools.limits.max_evidence_bytes, cancel)?;
                let (value, probe) = probe(tools, Value::String(text.clone()))?;
                let safe = value.as_str().ok_or_else(invalid)?;
                let (excerpt, excerpt_truncated) =
                    clip(safe, tools.limits.max_evidence_excerpt_bytes);
                Ok(Resolved {
                    info: MemoryEvidence {
                        selector: selector.clone(),
                        origin_scope,
                        source_reference: format!(
                            "session-history:{}:user:{ordinal}",
                            self.session.id.0
                        ),
                        revision,
                        state: EvidenceState::UserInput,
                        result_truncated: false,
                        excerpt: Value::String(excerpt),
                        excerpt_truncated,
                    },
                    probe,
                })
            }
            EvidenceSelector::ToolResult { call_id } => {
                if call_id.is_empty()
                    || call_id.len() > 256
                    || call_id.chars().any(char::is_control)
                {
                    return Err(invalid());
                }
                let calls = self.calls.get(call_id.as_str()).ok_or_else(invalid)?;
                let results = self.results.get(call_id.as_str()).ok_or_else(invalid)?;
                if calls.len() != 1 || results.len() != 1 || calls[0] >= results[0] {
                    return Err(invalid());
                }
                let request = &self.session.history[calls[0]];
                let observation = &self.session.history[results[0]];
                let HistoryItem::ToolCall(call) = request else {
                    return Err(invalid());
                };
                let HistoryItem::ToolResult { result, .. } = observation else {
                    return Err(invalid());
                };
                let revision = hash(
                    &(request, observation),
                    tools.limits.max_evidence_bytes,
                    cancel,
                )?;
                let state = if result
                    .error
                    .as_ref()
                    .is_some_and(|error| error.code.contains("unknown"))
                {
                    EvidenceState::Unknown
                } else if result.ok && result.error.is_none() {
                    EvidenceState::Succeeded
                } else {
                    EvidenceState::Failed
                };
                let (value, probe) = probe(
                    tools,
                    json!({"name":call.name,"arguments":call.arguments,"result":result}),
                )?;
                let safe_name = value.get("name").cloned().ok_or_else(invalid)?;
                let safe_arguments = value
                    .get("arguments")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?;
                let safe_result = encode(
                    value.get("result").ok_or_else(invalid)?,
                    tools.limits.max_evidence_bytes,
                )?;
                let safe_result = std::str::from_utf8(&safe_result).map_err(|_| invalid())?;
                let request_budget = tools.limits.max_evidence_excerpt_bytes / 4;
                let (arguments, request_truncated) = clip(safe_arguments, request_budget);
                let (result_text, response_truncated) = clip(
                    safe_result,
                    tools
                        .limits
                        .max_evidence_excerpt_bytes
                        .saturating_sub(request_budget),
                );
                Ok(Resolved {
                    info: MemoryEvidence {
                        selector: selector.clone(),
                        origin_scope,
                        source_reference: format!(
                            "session-history:{}:items:{}-{}",
                            self.session.id.0, calls[0], results[0]
                        ),
                        revision,
                        state,
                        result_truncated: result.truncated,
                        excerpt: json!({"name":safe_name,"arguments":arguments,"result":result_text}),
                        excerpt_truncated: request_truncated || response_truncated,
                    },
                    probe,
                })
            }
        }
    }
    fn is_memory_output(&self, selector: &EvidenceSelector) -> bool {
        let EvidenceSelector::ToolResult { call_id } = selector else {
            return false;
        };
        self.calls.get(call_id.as_str()).is_some_and(|positions|positions.iter().any(|position| {
            matches!(&self.session.history[*position],HistoryItem::ToolCall(call) if matches!(call.name.as_str(),"memory_read"|"memory_search"|"memory_update"|"memory_forget"|"memory_evidence"))
        }))
    }
}

pub(super) struct Bundle {
    pub(super) selected: BTreeMap<EvidenceSelector, MemoryEvidence>,
    pub(super) latest_user: Option<MemoryEvidence>,
    pub(super) history_digest: String,
    pub(super) probes: Vec<RedactionProbe>,
}
impl Bundle {
    pub(super) fn digest(&self, limits: ManagedMemoryLimits) -> Result<String, BoundaryError> {
        let identities = self
            .selected
            .values()
            .map(|evidence| {
                (
                    &evidence.selector,
                    &evidence.revision,
                    &evidence.origin_scope,
                )
            })
            .collect::<Vec<_>>();
        Ok(super::digest(&encode(
            &identities,
            limits.max_argument_bytes,
        )?))
    }
}
pub(super) fn selected(
    tools: &ManagedMemoryTools,
    session: &Session,
    selectors: impl IntoIterator<Item = EvidenceSelector>,
    cancel: &AtomicBool,
) -> Result<Bundle, BoundaryError> {
    let selectors = selectors.into_iter().collect::<BTreeSet<_>>();
    if selectors.len() > tools.limits.max_total_evidence {
        return Err(failure(
            "memory_evidence_limit",
            "Memory mutation selects too many evidence records",
        ));
    }
    let index = Index::new(session, cancel)?;
    let history_digest = hash(&session.history, tools.limits.max_history_bytes, cancel)?;
    let mut selected = BTreeMap::new();
    let mut probes = Vec::new();
    for selector in selectors {
        if index.is_memory_output(&selector) {
            return Err(failure(
                "memory_evidence_invalid",
                "Memory tool outputs cannot provide independent evidence for a new fact",
            ));
        }
        let resolved = index.resolve(tools, &selector, cancel)?;
        probes.push(resolved.probe);
        selected.insert(selector, resolved.info);
    }
    let latest_user = if index.users.is_empty() {
        None
    } else {
        let selector = EvidenceSelector::UserMessage {
            ordinal: index.users.len(),
        };
        Some(match selected.get(&selector) {
            Some(evidence) => evidence.clone(),
            None => {
                let resolved = index.resolve(tools, &selector, cancel)?;
                probes.push(resolved.probe);
                resolved.info
            }
        })
    };
    Ok(Bundle {
        selected,
        latest_user,
        history_digest,
        probes,
    })
}

pub(super) fn page(
    tools: &ManagedMemoryTools,
    session: &Session,
    args: &args::EvidenceArgs,
    cancel: &AtomicBool,
) -> Result<MemoryEvidencePage, BoundaryError> {
    let index = Index::new(session, cancel)?;
    let (snapshot_items, history_sha256, mut before) = match &args.cursor {
        Some(cursor) => {
            if cursor.session_id != session.id
                || cursor.snapshot_items > session.history.len()
                || cursor.before_item > cursor.snapshot_items
                || !valid_digest(&cursor.history_sha256)
            {
                return Err(failure(
                    "cursor_invalid",
                    "Memory evidence cursor belongs to another or changed session",
                ));
            }
            let hash = hash(
                &session.history[..cursor.snapshot_items],
                tools.limits.max_history_bytes,
                cancel,
            )?;
            if hash != cursor.history_sha256 {
                return Err(failure(
                    "cursor_invalid",
                    "Memory evidence history snapshot changed",
                ));
            }
            (cursor.snapshot_items, hash, cursor.before_item)
        }
        None => (
            session.history.len(),
            hash(&session.history, tools.limits.max_history_bytes, cancel)?,
            session.history.len(),
        ),
    };
    let mut page = MemoryEvidencePage {
        session_id: session.id,
        workspace_identity: tools.store.workspace().id.clone(),
        history_sha256: history_sha256.clone(),
        snapshot_items,
        entries: Vec::new(),
        excluded: 0,
        truncated: false,
        next_cursor: None,
    };
    let maximum = args.max_items.unwrap_or(32);
    while before > 0 && page.entries.len() < maximum {
        cancelled(cancel)?;
        before -= 1;
        let selector = match &session.history[before] {
            HistoryItem::UserText(text) if !text.trim().is_empty() => {
                let ordinal = index.users.binary_search(&before).map_err(|_| invalid())? + 1;
                EvidenceSelector::UserMessage { ordinal }
            }
            HistoryItem::ToolResult { call_id, .. } => EvidenceSelector::ToolResult {
                call_id: call_id.clone(),
            },
            _ => continue,
        };
        if index.is_memory_output(&selector) {
            page.excluded += 1;
            continue;
        }
        let resolved = match index.resolve(tools, &selector, cancel) {
            Ok(resolved) => resolved.info,
            Err(error)
                if matches!(
                    error.0.code.as_str(),
                    "memory_evidence_invalid" | "memory_evidence_limit"
                ) =>
            {
                page.excluded += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        let entry = MemoryEvidenceCatalogItem {
            selector: resolved.selector,
            revision: resolved.revision,
            state: resolved.state,
            result_truncated: resolved.result_truncated,
        };
        let value = serde_json::to_value(&entry).map_err(|_| invalid())?;
        if tools.store.redact_review_value(value.clone())? != value {
            page.excluded += 1;
            continue;
        }
        page.entries.push(entry);
        page.truncated = before > 0 || page.excluded > 0;
        page.next_cursor = (before > 0).then_some(MemoryEvidenceCursor {
            session_id: session.id,
            history_sha256: history_sha256.clone(),
            snapshot_items,
            before_item: before,
        });
        let result = ToolResult {
            ok: true,
            data: Some(serde_json::to_value(&page).map_err(|_| invalid())?),
            error: None,
            truncated: page.truncated,
        };
        if encode(&result, tools.limits.max_result_bytes).is_err() {
            page.entries.pop();
            before += 1;
            if page.entries.is_empty() {
                return Err(failure(
                    "memory_result_limit",
                    "Memory evidence page cannot fit the next selector",
                ));
            }
            break;
        }
    }
    page.truncated = before > 0 || page.excluded > 0;
    page.next_cursor = (before > 0).then_some(MemoryEvidenceCursor {
        session_id: session.id,
        history_sha256,
        snapshot_items,
        before_item: before,
    });
    cancelled(cancel)?;
    Ok(page)
}
pub(super) fn source(
    tools: &ManagedMemoryTools,
    evidence: &MemoryEvidence,
    scope: &MemoryScope,
) -> Result<MemorySource, BoundaryError> {
    if *scope == MemoryScope::Global && evidence.state != EvidenceState::UserInput {
        return Err(failure(
            "memory_scope",
            "Tool evidence remains scoped to its actual workspace",
        ));
    }
    let identity = encode(
        &(&evidence.source_reference, scope),
        tools.limits.max_argument_bytes,
    )?;
    Ok(MemorySource {
        id: format!("src_{}", digest(&identity)),
        kind: if evidence.state == EvidenceState::UserInput {
            MemorySourceKind::User
        } else {
            MemorySourceKind::Tool
        },
        scope: scope.clone(),
        reference: evidence.source_reference.clone(),
        revision: evidence.revision.clone(),
        observed_at: "not_recorded".into(),
        description: if evidence.state == EvidenceState::UserInput {
            "Persisted user message".into()
        } else {
            format!(
                "Persisted tool observation ({:?}, truncated={})",
                evidence.state, evidence.result_truncated
            )
        },
        metadata_withheld: false,
    })
}
pub(super) fn catalog(
    tools: &ManagedMemoryTools,
    session: &Session,
    cancel: &AtomicBool,
) -> Result<MemoryEvidenceCatalog, BoundaryError> {
    let index = Index::new(session, cancel)?;
    let mut candidates = VecDeque::new();
    let mut user_ordinal = 0;
    let maximum = tools.limits.max_total_evidence.min(32);
    let mut total = 0usize;
    for item in &session.history {
        cancelled(cancel)?;
        let selector = match item {
            HistoryItem::UserText(text) => {
                user_ordinal += 1;
                if text.trim().is_empty() {
                    continue;
                }
                EvidenceSelector::UserMessage {
                    ordinal: user_ordinal,
                }
            }
            HistoryItem::ToolResult { call_id, .. } => EvidenceSelector::ToolResult {
                call_id: call_id.clone(),
            },
            _ => continue,
        };
        total += 1;
        candidates.push_back(selector);
        if candidates.len() > maximum {
            candidates.pop_front();
        }
    }
    let mut entries = Vec::new();
    for selector in candidates {
        let evidence = match index.resolve(tools, &selector, cancel) {
            Ok(evidence) => evidence.info,
            Err(error) if error.0.code == "memory_evidence_invalid" => continue,
            Err(error) => return Err(error),
        };
        let entry = MemoryEvidenceCatalogItem {
            selector: evidence.selector,
            revision: evidence.revision,
            state: evidence.state,
            result_truncated: evidence.result_truncated,
        };
        let value = serde_json::to_value(&entry).map_err(|_| invalid())?;
        if tools.store.redact_review_value(value.clone())? == value {
            entries.push(entry);
        }
    }
    Ok(MemoryEvidenceCatalog {
        session_id: session.id,
        workspace_identity: tools.store.workspace().id.clone(),
        truncated: total > entries.len(),
        entries,
    })
}
