use super::*;
use std::collections::BTreeSet;

#[derive(Default)]
struct ForgetGroup {
    fact_ids: Vec<String>,
    fingerprints: BTreeSet<String>,
    source_ids: BTreeSet<String>,
    resources: BTreeSet<String>,
    entry_metadata: BTreeSet<(String, String, String)>,
}

/// The host validates source identities against their owners before proposing a
/// mutation. Constructing a proposal neither authorizes nor commits it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryChange {
    Upsert {
        entry: MemoryEntry,
        sources: Vec<MemorySource>,
    },
    Forget {
        scope: MemoryScope,
        fact_ids: Vec<String>,
    },
    RetractSource {
        source_id: String,
        revision: String,
    },
    RestoreVersion {
        version: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryProposal {
    pub operation_id: String,
    pub expected_revision: String,
    pub change_digest: String,
    pub proposed_document_digest: String,
    pub before_entries: Vec<MemoryEntry>,
    pub after_entries: Vec<MemoryEntry>,
    pub added_fact_ids: Vec<String>,
    pub removed_fact_ids: Vec<String>,
    #[serde(skip)]
    pub(super) document: MemoryDocument,
    #[serde(skip)]
    pub(super) change: MemoryChange,
    #[serde(skip)]
    pub(super) store_binding: String,
    #[serde(skip)]
    pub(super) before_index_digest: Option<String>,
    #[serde(skip)]
    pub(super) host_request_digest: Option<String>,
}
impl MemoryProposal {
    pub fn change(&self) -> &MemoryChange {
        &self.change
    }
    /// Complete candidate for host persistence and file review. It includes
    /// other workspace records preserved by the change. Model-facing review
    /// uses the scope-filtered before_entries and after_entries fields.
    pub fn document_for_host(&self) -> &MemoryDocument {
        &self.document
    }
    pub fn bind_host_request(mut self, host_request_digest: &str) -> Result<Self, BoundaryError> {
        if !valid_digest(host_request_digest) {
            return Err(error(
                "memory_args",
                "Host memory request digest is invalid",
            ));
        }
        self.host_request_digest = Some(host_request_digest.into());
        Ok(self)
    }
    pub fn host_request_digest(&self) -> Option<&str> {
        self.host_request_digest.as_deref()
    }
}

fn visible_scope(scope: &MemoryScope, workspace: &WorkspaceIdentity) -> Result<(), BoundaryError> {
    if !scope.visible_in(workspace) {
        return Err(error(
            "memory_scope",
            "Memory mutation targets another workspace",
        ));
    }
    Ok(())
}

pub(super) fn propose(
    store: &GlobalMemoryStore,
    expected_revision: &str,
    operation_id: &str,
    change: MemoryChange,
) -> Result<MemoryProposal, BoundaryError> {
    if !valid_id(operation_id, "op_") {
        return Err(error("memory_args", "Memory operation identity is invalid"));
    }
    if crate::redaction::contains_secret(operation_id, &store.secrets.snapshot()?) {
        return Err(error(
            "sensitive_data",
            "Memory operation identity contains protected data",
        ));
    }
    let encoded_change = encode_bounded(&change, state::max_state_bytes(store.limits))?;
    if crate::redaction::contains_secret(
        std::str::from_utf8(&encoded_change)
            .map_err(|_| error("memory_args", "Memory change encoding is invalid"))?,
        &store.secrets.snapshot()?,
    ) {
        return Err(error(
            "sensitive_data",
            "Memory change contains protected data",
        ));
    }
    let loaded = store.load()?;
    check_revision(Some(expected_revision), &loaded.snapshot.revision)?;
    let mut document = loaded.document.clone();
    match &change {
        MemoryChange::Upsert { entry, sources } => {
            if sources.len() > store.limits.max_sources
                || entry.facts.len() > store.limits.max_facts
            {
                return Err(error(
                    "memory_limit",
                    "Memory proposal exceeds its item limits",
                ));
            }
            visible_scope(&entry.scope, &store.workspace)?;
            if entry
                .facts
                .iter()
                .any(|fact| suppressed_by_current_state(&document, &entry.scope, fact))
            {
                return Err(error(
                    "memory_forgotten",
                    "Memory proposal conflicts with current forget or source-withdrawal state",
                ));
            }
            if document
                .entries
                .iter()
                .any(|old| old.id == entry.id && old.scope != entry.scope)
            {
                return Err(error(
                    "memory_scope",
                    "Memory entry scope cannot be reassigned",
                ));
            }
            let old_facts = document
                .entries
                .iter()
                .find(|old| old.id == entry.id)
                .into_iter()
                .flat_map(|entry| &entry.facts)
                .map(|fact| (fact.id.clone(), fact.clone()))
                .collect::<BTreeMap<_, _>>();
            let fresh = entry
                .facts
                .iter()
                .filter(|fact| old_facts.get(&fact.id) != Some(*fact))
                .map(|fact| fact.id.clone())
                .collect::<BTreeSet<_>>();
            let changed = old_facts
                .values()
                .filter(|fact| entry.facts.iter().find(|next| next.id == fact.id) != Some(*fact))
                .map(|fact| fact.id.clone())
                .collect::<BTreeSet<_>>();
            for source in sources {
                visible_scope(&source.scope, &store.workspace)?;
                if let Some(old) = document.sources.iter_mut().find(|old| old.id == source.id) {
                    if old.scope != source.scope {
                        return Err(error(
                            "memory_scope",
                            "Memory source scope cannot be reassigned",
                        ));
                    }
                    *old = source.clone();
                } else {
                    document.sources.push(source.clone());
                }
            }
            if let Some(old) = document.entries.iter_mut().find(|old| old.id == entry.id) {
                *old = entry.clone();
            } else {
                document.entries.push(entry.clone());
            }
            let invalidated = invalidate_derived(&mut document, changed, &fresh);
            let used = document
                .entries
                .iter()
                .flat_map(|entry| &entry.facts)
                .flat_map(|fact| &fact.sources)
                .map(|source| source.source_id.clone())
                .collect::<BTreeSet<_>>();
            document.sources.retain(|source| used.contains(&source.id));
            document::validate(&document, store.limits, &store.secrets.snapshot()?)?;
            let active = document::active_document(&document);
            if entry.facts.iter().any(|fact| {
                !invalidated.contains(&fact.id)
                    && !active
                        .entries
                        .iter()
                        .flat_map(|entry| &entry.facts)
                        .any(|current| current.id == fact.id)
            }) {
                return Err(error(
                    "memory_forgotten",
                    "Memory proposal conflicts with current forget or source-withdrawal state",
                ));
            }
        }
        MemoryChange::Forget { scope, fact_ids } => {
            visible_scope(scope, &store.workspace)?;
            if fact_ids.is_empty()
                || fact_ids.len() > store.limits.max_facts
                || fact_ids.iter().any(|id| !valid_id(id, "fact_"))
            {
                return Err(error("memory_args", "Memory forget targets are invalid"));
            }
            let active = document::active_document(&document);
            let mut fingerprints = BTreeSet::new();
            let mut matched = BTreeSet::new();
            for entry in &active.entries {
                if &entry.scope != scope {
                    continue;
                }
                for fact in &entry.facts {
                    if fact_ids.contains(&fact.id) {
                        matched.insert(fact.id.clone());
                        fingerprints.insert(fact_fingerprint(&fact.text));
                        fingerprints
                            .extend(fact.aliases.iter().map(|alias| fact_fingerprint(alias)));
                    }
                }
            }
            if fact_ids.iter().any(|id| !matched.contains(id)) {
                return Err(error(
                    "memory_not_found",
                    "Memory forget target is not active in its declared scope",
                ));
            }
            let mut removed = BTreeSet::new();
            let mut by_scope = BTreeMap::from([(scope.clone(), fingerprints)]);
            loop {
                let previous = removed.len();
                for entry in &active.entries {
                    for fact in &entry.facts {
                        let duplicate = by_scope.get(&entry.scope).is_some_and(|fingerprints| {
                            fingerprints.contains(&fact_fingerprint(&fact.text))
                                || fact
                                    .aliases
                                    .iter()
                                    .any(|alias| fingerprints.contains(&fact_fingerprint(alias)))
                        });
                        if (matched.contains(&fact.id)
                            || duplicate
                            || fact
                                .derived_from
                                .iter()
                                .any(|parent| removed.contains(parent)))
                            && removed.insert(fact.id.clone())
                        {
                            let fingerprints = by_scope.entry(entry.scope.clone()).or_default();
                            fingerprints.insert(fact_fingerprint(&fact.text));
                            fingerprints
                                .extend(fact.aliases.iter().map(|alias| fact_fingerprint(alias)));
                        }
                    }
                }
                if removed.len() == previous {
                    break;
                }
            }
            let mut groups = BTreeMap::<MemoryScope, ForgetGroup>::new();
            for entry in &active.entries {
                for fact in &entry.facts {
                    if removed.contains(&fact.id) {
                        let group = groups.entry(entry.scope.clone()).or_default();
                        group.fact_ids.push(fact.id.clone());
                        group.fingerprints.insert(fact_fingerprint(&fact.text));
                        group
                            .fingerprints
                            .extend(fact.aliases.iter().map(|alias| fact_fingerprint(alias)));
                        group
                            .source_ids
                            .extend(fact.sources.iter().map(|source| source.source_id.clone()));
                        group.resources.extend(entry.resources.iter().cloned());
                        group.entry_metadata.insert((
                            entry.id.clone(),
                            fact_fingerprint(&entry.title),
                            fact_fingerprint(&entry.updated_at),
                        ));
                    }
                }
            }
            for (scope, group) in groups {
                document.tombstones.push(MemoryTombstone {
                    operation_id: operation_id.into(),
                    scope,
                    fact_ids: group.fact_ids,
                    fingerprints: group.fingerprints.into_iter().collect(),
                    source_ids: group.source_ids.into_iter().collect(),
                    resources: group.resources.into_iter().collect(),
                    entry_metadata: group
                        .entry_metadata
                        .into_iter()
                        .map(|(entry_id, title_fingerprint, updated_at_fingerprint)| {
                            MemoryEntryMetadataSuppression {
                                entry_id,
                                title_fingerprint,
                                updated_at_fingerprint: Some(updated_at_fingerprint),
                            }
                        })
                        .collect(),
                });
            }
            let affected_sources = active
                .entries
                .iter()
                .flat_map(|entry| &entry.facts)
                .filter(|fact| removed.contains(&fact.id))
                .flat_map(|fact| fact.sources.iter().map(|source| source.source_id.clone()))
                .collect::<BTreeSet<_>>();
            for source in &mut document.sources {
                if affected_sources.contains(&source.id) {
                    source.description.clear();
                }
            }
        }
        MemoryChange::RetractSource {
            source_id,
            revision,
        } => {
            let source = document
                .sources
                .iter()
                .find(|source| &source.id == source_id && &source.revision == revision)
                .ok_or_else(|| error("memory_source", "Memory source version is unknown"))?;
            visible_scope(&source.scope, &store.workspace)?;
            if !document
                .retractions
                .iter()
                .any(|old| old.source_id == *source_id && old.revision == *revision)
            {
                let before = document::active_document(&document);
                let retraction = SourceRetraction {
                    source_id: source_id.clone(),
                    revision: revision.clone(),
                    scope: source.scope.clone(),
                    resources: Vec::new(),
                    entry_metadata: Vec::new(),
                };
                document.retractions.push(retraction);
                let after = document::active_document(&document);
                let affected = before
                    .entries
                    .iter()
                    .filter(|entry| {
                        after
                            .entries
                            .iter()
                            .find(|current| current.id == entry.id)
                            .is_none_or(|current| current.facts.len() != entry.facts.len())
                    })
                    .collect::<Vec<_>>();
                let resources = affected
                    .iter()
                    .flat_map(|entry| entry.resources.iter().cloned())
                    .collect::<BTreeSet<_>>();
                let entry_metadata = affected
                    .iter()
                    .map(|entry| MemoryEntryMetadataSuppression {
                        entry_id: entry.id.clone(),
                        title_fingerprint: fact_fingerprint(&entry.title),
                        updated_at_fingerprint: Some(fact_fingerprint(&entry.updated_at)),
                    })
                    .collect();
                if let Some(retraction) = document.retractions.last_mut() {
                    retraction.resources = resources.into_iter().collect();
                    retraction.entry_metadata = entry_metadata;
                }
            }
        }
        MemoryChange::RestoreVersion { version } => {
            let mut restored = store.history_document(version)?;
            // An index rollback can restore only records visible in this
            // workspace. Other workspace records remain at their current view.
            restored
                .entries
                .retain(|entry| entry.scope.visible_in(&store.workspace));
            let source_ids = restored
                .entries
                .iter()
                .flat_map(|entry| entry.facts.iter())
                .flat_map(|fact| fact.sources.iter().map(|source| source.source_id.clone()))
                .collect::<BTreeSet<_>>();
            restored
                .sources
                .retain(|source| source_ids.contains(&source.id));
            let foreign = document
                .entries
                .iter()
                .filter(|entry| !entry.scope.visible_in(&store.workspace))
                .cloned()
                .collect::<Vec<_>>();
            let foreign_sources = foreign
                .iter()
                .flat_map(|entry| entry.facts.iter())
                .flat_map(|fact| fact.sources.iter().map(|source| source.source_id.clone()))
                .collect::<BTreeSet<_>>();
            for source in document
                .sources
                .iter()
                .filter(|source| foreign_sources.contains(&source.id))
            {
                match restored.sources.iter_mut().find(|old| old.id == source.id) {
                    Some(old) if old.revision != source.revision || old.scope != source.scope => {
                        return Err(error(
                            "memory_source",
                            "History source conflicts with a currently shared source",
                        ));
                    }
                    Some(old) => *old = source.clone(),
                    None => restored.sources.push(source.clone()),
                }
            }
            restored.entries.extend(foreign);
            let current = state::Ledger {
                tombstones: document.tombstones.clone(),
                retractions: document.retractions.clone(),
                ..state::Ledger::default()
            };
            state::merge(&mut restored, &current)?;
            document = restored;
        }
    }
    document::validate(&document, store.limits, &store.secrets.snapshot()?)?;
    document = document::active_document(&document);
    let used_sources = document
        .entries
        .iter()
        .flat_map(|entry| &entry.facts)
        .flat_map(|fact| &fact.sources)
        .map(|source| source.source_id.clone())
        .collect::<BTreeSet<_>>();
    document
        .sources
        .retain(|source| used_sources.contains(&source.id));
    let rendered = store.render_index(&document)?;
    let before_entries = loaded.snapshot.entries;
    let after_entries = document
        .entries
        .iter()
        .filter(|entry| entry.scope.visible_in(&store.workspace))
        .cloned()
        .collect::<Vec<_>>();
    let before = before_entries
        .iter()
        .flat_map(|entry| entry.facts.iter().map(|fact| fact.id.clone()))
        .collect::<BTreeSet<_>>();
    let after = after_entries
        .iter()
        .flat_map(|entry| entry.facts.iter().map(|fact| fact.id.clone()))
        .collect::<BTreeSet<_>>();
    Ok(MemoryProposal {
        operation_id: operation_id.into(),
        expected_revision: expected_revision.into(),
        change_digest: digest(&encoded_change),
        proposed_document_digest: digest(rendered.as_bytes()),
        before_entries,
        after_entries,
        added_fact_ids: after.difference(&before).cloned().collect(),
        removed_fact_ids: before.difference(&after).cloned().collect(),
        document,
        change,
        store_binding: store.binding(),
        before_index_digest: loaded.index_digest,
        host_request_digest: None,
    })
}

fn suppressed_by_current_state(
    document: &MemoryDocument,
    scope: &MemoryScope,
    fact: &MemoryFact,
) -> bool {
    let forgotten = document.tombstones.iter().any(|stone| {
        (stone.scope == *scope
            && (stone.fact_ids.contains(&fact.id)
                || stone.fingerprints.contains(&fact_fingerprint(&fact.text))
                || fact
                    .aliases
                    .iter()
                    .any(|alias| stone.fingerprints.contains(&fact_fingerprint(alias)))))
            || (stone.scope.supports(scope)
                && fact
                    .derived_from
                    .iter()
                    .any(|id| stone.fact_ids.contains(id)))
    });
    let withdrawn = !fact.sources.is_empty()
        && fact.sources.iter().all(|source| {
            document.retractions.iter().any(|retraction| {
                retraction.scope.supports(scope)
                    && retraction.source_id == source.source_id
                    && retraction.revision == source.revision
            })
        });
    forgotten || withdrawn
}

fn invalidate_derived(
    document: &mut MemoryDocument,
    mut changed: BTreeSet<String>,
    fresh: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut removed = BTreeSet::new();
    loop {
        let before = removed.len();
        for fact in document.entries.iter().flat_map(|entry| &entry.facts) {
            if !fresh.contains(&fact.id)
                && fact
                    .derived_from
                    .iter()
                    .any(|parent| changed.contains(parent))
            {
                removed.insert(fact.id.clone());
                changed.insert(fact.id.clone());
            }
        }
        if removed.len() == before {
            break;
        }
    }
    for entry in &mut document.entries {
        let before = entry.facts.len();
        entry.facts.retain(|fact| !removed.contains(&fact.id));
        if entry.facts.len() != before {
            entry.resources.clear();
        }
    }
    document.entries.retain(|entry| !entry.facts.is_empty());
    removed
}
