//! Versioned administrative backups of the current visible active view.
//! Coverage includes topic references and current forget constraints. Original
//! evidence, historical versions and topic material retain their own owners.
use super::*;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryBackupCoverage {
    pub active_global_and_current_workspace: bool,
    pub source_metadata: bool,
    pub current_forgetting_constraints: bool,
    pub topic_references: bool,
    pub topic_bodies: bool,
    pub historical_versions: bool,
    pub original_session_evidence: bool,
}
impl Default for MemoryBackupCoverage {
    fn default() -> Self {
        Self {
            active_global_and_current_workspace: true,
            source_metadata: true,
            current_forgetting_constraints: true,
            topic_references: true,
            topic_bodies: false,
            historical_versions: false,
            original_session_evidence: false,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryBackupV1 {
    pub schema_version: u32,
    pub workspace: WorkspaceIdentity,
    pub source_revision: String,
    pub forget_revision: String,
    pub coverage: MemoryBackupCoverage,
    pub document: MemoryDocument,
    pub resources: Vec<MemoryResourceInfo>,
    pub checksum: String,
}
fn max_bytes(limits: MemoryLimits) -> usize {
    // Reserve the surrounding MemoryChange envelope so every accepted backup
    // fits the existing proposal and commit request bound.
    state::max_state_bytes(limits).saturating_sub(1024)
}
impl MemoryBackupV1 {
    fn checksum(&self, limits: MemoryLimits) -> Result<String, BoundaryError> {
        let mut normalized = self.clone();
        normalized.checksum.clear();
        Ok(digest(&encode_bounded(&normalized, max_bytes(limits))?))
    }
    fn validate(&self, store: &GlobalMemoryStore) -> Result<(), BoundaryError> {
        if self.schema_version != 1
            || self.coverage != MemoryBackupCoverage::default()
            || !valid_digest(&self.source_revision)
            || !valid_digest(&self.forget_revision)
            || !valid_digest(&self.checksum)
            || self.checksum != self.checksum(store.limits)?
        {
            return Err(error(
                "memory_backup_invalid",
                "Memory backup version, coverage or checksum is invalid",
            ));
        }
        self.workspace.validate()?;
        document::validate(&self.document, store.limits, &store.secrets.snapshot()?)?;
        let active = document::active_document(&self.document);
        if active.entries != self.document.entries || active.sources != self.document.sources {
            return Err(error(
                "memory_backup_invalid",
                "Memory backup contains withdrawn active records",
            ));
        }
        let scope_visible = |scope: &MemoryScope| scope.visible_in(&self.workspace);
        if self
            .document
            .entries
            .iter()
            .any(|entry| !scope_visible(&entry.scope))
            || self
                .document
                .sources
                .iter()
                .any(|source| !scope_visible(&source.scope))
            || self
                .document
                .tombstones
                .iter()
                .any(|stone| !scope_visible(&stone.scope))
            || self
                .document
                .retractions
                .iter()
                .any(|source| !scope_visible(&source.scope))
        {
            return Err(error(
                "memory_scope",
                "Memory backup exceeds its declared workspace view",
            ));
        }
        let has_workspace = self
            .document
            .entries
            .iter()
            .any(|entry| entry.scope != MemoryScope::Global)
            || self
                .document
                .sources
                .iter()
                .any(|source| source.scope != MemoryScope::Global)
            || self
                .document
                .tombstones
                .iter()
                .any(|stone| stone.scope != MemoryScope::Global)
            || self
                .document
                .retractions
                .iter()
                .any(|source| source.scope != MemoryScope::Global);
        if has_workspace && self.workspace != store.workspace {
            return Err(error(
                "memory_scope",
                "Memory backup belongs to another workspace",
            ));
        }
        let paths = self
            .document
            .entries
            .iter()
            .flat_map(|entry| entry.resources.iter().map(String::as_str))
            .collect::<BTreeSet<_>>();
        let mut supplied = BTreeSet::new();
        let mut total = 0usize;
        for resource in &self.resources {
            disk::validate_resource_path(&resource.path)?;
            total = total
                .checked_add(resource.bytes)
                .ok_or_else(|| error("memory_limit", "Memory backup topic sizes overflow"))?;
            if !valid_digest(&resource.revision)
                || resource.bytes > store.limits.max_resource_bytes
                || !supplied.insert(resource.path.as_str())
            {
                return Err(error(
                    "memory_backup_invalid",
                    "Memory backup topic metadata is invalid",
                ));
            }
        }
        if self.resources.len() > store.limits.max_resources
            || total > store.limits.max_total_resource_bytes
            || paths != supplied
        {
            return Err(error(
                "memory_backup_invalid",
                "Memory backup topic references do not match its active entries",
            ));
        }
        let _ = encode_bounded(self, max_bytes(store.limits))?;
        Ok(())
    }
}

impl GlobalMemoryStore {
    /// Stable identity for an explicit administrative request. The caller
    /// supplies normalized typed arguments and keeps authorization separate.
    pub fn administrative_request_digest(
        &self,
        request: &serde_json::Value,
    ) -> Result<String, BoundaryError> {
        if self.redact_review_value(request.clone())? != *request {
            return Err(error(
                "sensitive_data",
                "Administrative memory request contains protected data",
            ));
        }
        Ok(digest(&encode_bounded(
            &("saya-memory-admin-v1", self.binding(), request),
            state::max_state_bytes(self.limits),
        )?))
    }
    pub fn export_backup(&self) -> Result<MemoryBackupV1, BoundaryError> {
        let loaded = self.load()?;
        let current = document::active_document(&loaded.document);
        let document = MemoryDocument {
            schema_version: 1,
            entries: loaded.snapshot.entries.clone(),
            sources: loaded.snapshot.sources.clone(),
            tombstones: current
                .tombstones
                .into_iter()
                .filter(|stone| stone.scope.visible_in(&self.workspace))
                .collect(),
            retractions: current
                .retractions
                .into_iter()
                .filter(|source| source.scope.visible_in(&self.workspace))
                .collect(),
        };
        let mut backup = MemoryBackupV1 {
            schema_version: 1,
            workspace: self.workspace.clone(),
            source_revision: loaded.snapshot.revision,
            forget_revision: loaded.snapshot.forget_revision,
            coverage: MemoryBackupCoverage::default(),
            document,
            resources: loaded.snapshot.resources,
            checksum: String::new(),
        };
        backup.checksum = backup.checksum(self.limits)?;
        backup.validate(self)?;
        Ok(backup)
    }
    pub fn parse_backup(&self, text: &str) -> Result<MemoryBackupV1, BoundaryError> {
        if text.len() > max_bytes(self.limits) {
            return Err(error(
                "memory_limit",
                "Memory backup exceeds its byte limit",
            ));
        }
        if crate::redaction::contains_secret(text, &self.secrets.snapshot()?) {
            return Err(error(
                "sensitive_data",
                "Memory backup contains protected data",
            ));
        }
        let backup: MemoryBackupV1 = serde_json::from_str(text)
            .map_err(|_| error("memory_backup_invalid", "Memory backup format is invalid"))?;
        backup.validate(self)?;
        Ok(backup)
    }
    /// Prepare one atomic replacement of the visible entry set. Current hidden
    /// workspaces and current forgetting constraints are retained by the host.
    /// Topic bodies must already exist with the exported digest and are read
    /// through the same configured-root checks as ordinary memory resources.
    pub fn propose_document_restore(
        &self,
        expected_revision: &str,
        operation_id: &str,
        backup: MemoryBackupV1,
    ) -> Result<MemoryProposal, BoundaryError> {
        self.propose(
            expected_revision,
            operation_id,
            MemoryChange::RestoreDocument { backup },
        )
    }
}

pub(super) fn reconcile(
    store: &GlobalMemoryStore,
    current: &MemoryDocument,
    backup: &MemoryBackupV1,
) -> Result<MemoryDocument, BoundaryError> {
    backup.validate(store)?;
    let current_active = document::active_document(current);
    let mut restored = backup.document.clone();
    let foreign = current_active
        .entries
        .iter()
        .filter(|entry| !entry.scope.visible_in(&store.workspace))
        .cloned()
        .collect::<Vec<_>>();
    let foreign_sources = foreign
        .iter()
        .flat_map(|entry| &entry.facts)
        .flat_map(|fact| &fact.sources)
        .map(|source| source.source_id.as_str())
        .collect::<BTreeSet<_>>();
    for source in current_active
        .sources
        .iter()
        .filter(|source| foreign_sources.contains(source.id.as_str()))
    {
        match restored.sources.iter_mut().find(|old| old.id == source.id) {
            Some(old) if old.revision != source.revision || old.scope != source.scope => {
                return Err(error(
                    "memory_source",
                    "Restored source conflicts with a current hidden workspace source",
                ));
            }
            Some(old) => *old = source.clone(),
            None => restored.sources.push(source.clone()),
        }
    }
    restored.entries.extend(foreign);
    state::merge(
        &mut restored,
        &state::Ledger {
            tombstones: current.tombstones.clone(),
            retractions: current.retractions.clone(),
            ..state::Ledger::default()
        },
    )?;
    document::validate(&restored, store.limits, &store.secrets.snapshot()?)?;
    restored = document::active_document(&restored);
    let required = restored
        .entries
        .iter()
        .filter(|entry| entry.scope.visible_in(&store.workspace))
        .flat_map(|entry| entry.resources.iter())
        .collect::<BTreeSet<_>>();
    let root = disk::open_root(&store.root)?;
    for path in required {
        let reference = backup
            .resources
            .iter()
            .find(|resource| &resource.path == path)
            .ok_or_else(|| {
                error(
                    "memory_backup_invalid",
                    "Restored topic has no exported reference",
                )
            })?;
        let bytes = root
            .as_ref()
            .ok_or_else(|| {
                error(
                    "memory_resource_unavailable",
                    "Referenced topic must exist before memory restore",
                )
            })?
            .read(path, store.limits.max_resource_bytes)?
            .ok_or_else(|| {
                error(
                    "memory_resource_unavailable",
                    "Referenced topic must exist before memory restore",
                )
            })?;
        if bytes.len() != reference.bytes || digest(&bytes) != reference.revision {
            return Err(error(
                "memory_resource_changed",
                "Referenced topic differs from the exported version",
            ));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| error("memory_format", "Restored topic must be UTF-8"))?;
        if crate::redaction::contains_secret(text, &store.secrets.snapshot()?) {
            return Err(error(
                "sensitive_data",
                "Restored topic contains protected data",
            ));
        }
    }
    Ok(restored)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{code, document, fact, save, store};
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn backup_round_trip_restores_current_visible_entries_without_creating_on_read() {
        let temp = TempDir::new().unwrap();
        let original = store(&temp);
        save(&original, &document());
        let backup = original.export_backup().unwrap();
        assert!(!backup.coverage.topic_bodies);
        assert!(!backup.coverage.historical_versions);
        assert!(valid_digest(&backup.checksum));
        let restored = GlobalMemoryStore::open(
            &temp.path().join("restored"),
            original.workspace().clone(),
            original.limits(),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap();
        let encoded = serde_json::to_string(&backup).unwrap();
        let backup = restored.parse_backup(&encoded).unwrap();
        assert!(!restored.root().exists());
        let proposal = restored
            .propose_document_restore(&restored.snapshot().unwrap().revision, "op_restore", backup)
            .unwrap();
        assert!(!restored.root().exists());
        assert_eq!(
            restored.commit(&proposal).unwrap().status,
            MemoryCommitStatus::Committed
        );
        assert_eq!(
            restored.snapshot().unwrap().entries[0].facts[0].text,
            document().entries[0].facts[0].text
        );
    }
    #[test]
    fn restore_preserves_hidden_workspaces_and_current_forgetting_constraints() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp);
        let mut original = document();
        let mut hidden = original.entries[0].clone();
        hidden.id = "mem_hidden".into();
        hidden.scope = MemoryScope::Workspace {
            identity: format!("ws_{}", digest(b"another workspace")),
        };
        hidden.facts[0] = fact(
            "fact_hidden",
            "A hidden workspace fact.",
            &original.sources[0],
        );
        original.entries.push(hidden.clone());
        save(&store, &original);
        let backup = store.export_backup().unwrap();
        assert_eq!(backup.document.entries.len(), 1);
        let forget = store
            .propose(
                &store.snapshot().unwrap().revision,
                "op_forget",
                MemoryChange::Forget {
                    scope: MemoryScope::Global,
                    fact_ids: vec!["fact_editor".into()],
                },
            )
            .unwrap();
        assert_eq!(
            store.commit(&forget).unwrap().status,
            MemoryCommitStatus::Committed
        );
        let restore = store
            .propose_document_restore(
                &store.snapshot().unwrap().revision,
                "op_restore_old",
                backup,
            )
            .unwrap();
        assert!(restore.after_entries.is_empty());
        assert!(
            restore
                .document_for_host()
                .entries
                .iter()
                .any(|entry| entry.id == hidden.id && entry.facts == hidden.facts)
        );
        assert_eq!(
            store.commit(&restore).unwrap().status,
            MemoryCommitStatus::Committed
        );
        assert!(store.snapshot().unwrap().entries.is_empty());
    }
    #[test]
    fn altered_backup_and_missing_or_changed_topics_fail_before_publication() {
        let temp = TempDir::new().unwrap();
        let store = store(&temp);
        let mut original = document();
        original.entries[0].resources.push("memory/topic.md".into());
        save(&store, &original);
        std::fs::create_dir(store.root().join("memory")).unwrap();
        std::fs::write(store.root().join("memory/topic.md"), "Original topic.").unwrap();
        let backup = store.export_backup().unwrap();
        let mut edited = serde_json::to_value(&backup).unwrap();
        edited["document"]["entries"][0]["title"] = "Edited backup".into();
        assert_eq!(
            code(store.parse_backup(&edited.to_string())),
            "memory_backup_invalid"
        );
        let new_root = GlobalMemoryStore::open(
            &temp.path().join("new-root"),
            store.workspace().clone(),
            store.limits(),
            Arc::new(SecretRegistry::default()),
        )
        .unwrap();
        assert_eq!(
            code(new_root.propose_document_restore(
                &new_root.snapshot().unwrap().revision,
                "op_missing_topic",
                backup.clone()
            )),
            "memory_resource_unavailable"
        );
        assert!(!new_root.root().exists());
        std::fs::write(store.root().join("memory/topic.md"), "Changed topic.").unwrap();
        assert_eq!(
            code(store.propose_document_restore(
                &store.snapshot().unwrap().revision,
                "op_changed_topic",
                backup
            )),
            "memory_resource_changed"
        );
    }
}
