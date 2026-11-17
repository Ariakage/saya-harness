use super::*;

pub(super) const STATE_PATH: &str = "memory/.saya-state.json";
pub(super) const JOURNAL_PATH: &str = "memory/.saya-journal.json";
pub(super) const STAGE_PATH: &str = "memory/.saya-next.md";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryOperationReceipt {
    pub operation_id: String,
    pub change_digest: String,
    pub generation: u64,
    pub before_revision: String,
    pub document_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_request_digest: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryHistoryVersion {
    pub version: String,
    pub generation: u64,
    pub bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Ledger {
    pub(super) schema_version: u32,
    pub(super) generation: u64,
    pub(super) tombstones: Vec<MemoryTombstone>,
    pub(super) retractions: Vec<SourceRetraction>,
    pub(super) operations: Vec<MemoryOperationReceipt>,
    pub(super) history: Vec<MemoryHistoryVersion>,
}
impl Default for Ledger {
    fn default() -> Self {
        Self {
            schema_version: 1,
            generation: 0,
            tombstones: Vec::new(),
            retractions: Vec::new(),
            operations: Vec::new(),
            history: Vec::new(),
        }
    }
}
impl Ledger {
    pub(super) fn validate(
        &self,
        limits: MemoryLimits,
        secrets: &[String],
    ) -> Result<(), BoundaryError> {
        if self.schema_version != 1
            || self.operations.len() > limits.max_facts.saturating_mul(4)
            || self.history.len() > limits.max_history_versions
        {
            return Err(error(
                "memory_state",
                "Memory ledger version or limits are invalid",
            ));
        }
        let constraints = MemoryDocument {
            tombstones: self.tombstones.clone(),
            retractions: self.retractions.clone(),
            ..MemoryDocument::default()
        };
        document::validate(&constraints, limits, secrets)?;
        let mut operations = std::collections::BTreeSet::new();
        let mut generation = 0;
        for operation in &self.operations {
            if !valid_id(&operation.operation_id, "op_")
                || !operations.insert(&operation.operation_id)
                || !valid_digest(&operation.change_digest)
                || !valid_digest(&operation.before_revision)
                || !valid_digest(&operation.document_digest)
                || operation
                    .host_request_digest
                    .as_ref()
                    .is_some_and(|value| !valid_digest(value))
                || operation.generation <= generation
                || operation.generation > self.generation
            {
                return Err(error("memory_state", "Memory operation ledger is invalid"));
            }
            generation = operation.generation;
        }
        let mut versions = std::collections::BTreeSet::new();
        for version in &self.history {
            if !valid_digest(&version.version)
                || !versions.insert(&version.version)
                || version.generation > self.generation
                || version.bytes > limits.max_index_bytes
            {
                return Err(error("memory_state", "Memory history ledger is invalid"));
            }
        }
        if self.generation != generation {
            return Err(error(
                "memory_state",
                "Memory generation does not match its operation ledger",
            ));
        }
        let encoded = encode_bounded(self, max_state_bytes(limits))?;
        let text = std::str::from_utf8(&encoded)
            .map_err(|_| error("memory_state", "Memory ledger encoding is invalid"))?;
        if crate::redaction::contains_secret(text, secrets) {
            return Err(error(
                "sensitive_data",
                "Memory ledger contains protected data",
            ));
        }
        Ok(())
    }
}

pub(super) fn max_state_bytes(limits: MemoryLimits) -> usize {
    limits.max_index_bytes.saturating_mul(8)
}

pub(super) fn load(
    root: Option<&disk::MemoryRoot>,
    limits: MemoryLimits,
    secrets: &[String],
) -> Result<(Ledger, Option<Vec<u8>>), BoundaryError> {
    let raw = match root {
        Some(root) => root.read(STATE_PATH, max_state_bytes(limits))?,
        None => None,
    };
    let state = match &raw {
        Some(raw) => {
            if crate::redaction::contains_secret(
                std::str::from_utf8(raw)
                    .map_err(|_| error("memory_state", "Memory ledger must be UTF-8"))?,
                secrets,
            ) {
                return Err(error(
                    "sensitive_data",
                    "Memory ledger contains protected data",
                ));
            }
            serde_json::from_slice::<Ledger>(raw)
                .map_err(|_| error("memory_state", "Memory ledger cannot be parsed"))?
        }
        None => Ledger::default(),
    };
    state.validate(limits, secrets)?;
    Ok((state, raw))
}

pub(super) fn merge(document: &mut MemoryDocument, ledger: &Ledger) -> Result<(), BoundaryError> {
    for stone in &ledger.tombstones {
        match document.tombstones.iter().find(|current| {
            current.operation_id == stone.operation_id && current.scope == stone.scope
        }) {
            Some(current) if current != stone => {
                return Err(error(
                    "memory_state",
                    "Memory tombstone conflicts with its durable ledger",
                ));
            }
            Some(_) => {}
            None => document.tombstones.push(stone.clone()),
        }
    }
    for retraction in &ledger.retractions {
        match document.retractions.iter_mut().find(|current| {
            current.source_id == retraction.source_id && current.revision == retraction.revision
        }) {
            Some(current) => {
                if current.scope != retraction.scope {
                    return Err(error(
                        "memory_state",
                        "Memory retraction scope conflicts with its durable ledger",
                    ));
                }
                for path in &retraction.resources {
                    if !current.resources.contains(path) {
                        current.resources.push(path.clone());
                    }
                }
                for title in &retraction.entry_metadata {
                    if !current.entry_metadata.contains(title) {
                        current.entry_metadata.push(title.clone());
                    }
                }
            }
            None => document.retractions.push(retraction.clone()),
        }
    }
    Ok(())
}
