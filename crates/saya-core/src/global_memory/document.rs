use super::*;
use std::collections::{BTreeSet, VecDeque};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    schema_version: u32,
    sources: Vec<MemorySource>,
    tombstones: Vec<MemoryTombstone>,
    retractions: Vec<SourceRetraction>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryHeader {
    id: String,
    scope: MemoryScope,
    updated_at: String,
    resources: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FactHeader {
    id: String,
    kind: MemoryKind,
    sources: Vec<MemorySourceRef>,
    aliases: Vec<String>,
    derived_from: Vec<String>,
}

fn metadata<T: Serialize>(kind: &str, value: &T) -> Result<String, BoundaryError> {
    let json = serde_json::to_string(value)
        .map_err(|_| error("memory_format", "Memory metadata cannot be encoded"))?
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e");
    Ok(format!("<!-- saya-{kind} {json} -->\n"))
}
fn read_metadata<T: for<'a> Deserialize<'a>>(line: &str, kind: &str) -> Result<T, BoundaryError> {
    let prefix = format!("<!-- saya-{kind} ");
    let json = line
        .strip_prefix(&prefix)
        .and_then(|line| line.strip_suffix(" -->"))
        .ok_or_else(|| error("memory_format", "Memory metadata marker is invalid"))?;
    serde_json::from_str(json)
        .map_err(|_| error("memory_format", "Memory metadata is invalid or unsupported"))
}
fn line<'a>(text: &mut &'a str) -> &'a str {
    let (next, remaining) = (*text).split_once('\n').unwrap_or((*text, ""));
    *text = remaining;
    next.strip_suffix('\r').unwrap_or(next)
}
fn skip_blank(text: &mut &str) {
    while text.starts_with('\n') || text.starts_with("\r\n") {
        let _ = line(text);
    }
}

pub(super) fn parse(
    text: &str,
    limits: MemoryLimits,
    secrets: &[String],
) -> Result<MemoryDocument, BoundaryError> {
    if text.len() > limits.max_index_bytes {
        return Err(error("memory_limit", "Memory index exceeds its byte limit"));
    }
    if crate::redaction::contains_secret(text, secrets) {
        return Err(error(
            "sensitive_data",
            "Memory index contains protected data",
        ));
    }
    let mut remaining = text;
    if line(&mut remaining) != "# Saya Memory" {
        return Err(error("memory_format", "Memory index heading is missing"));
    }
    skip_blank(&mut remaining);
    let header: Header = read_metadata(line(&mut remaining), "memory")?;
    let mut document = MemoryDocument {
        schema_version: header.schema_version,
        sources: header.sources,
        entries: Vec::new(),
        tombstones: header.tombstones,
        retractions: header.retractions,
    };
    skip_blank(&mut remaining);
    while !remaining.is_empty() {
        if document.entries.len() >= limits.max_entries {
            return Err(error("memory_limit", "Memory entry limit exceeded"));
        }
        let title = line(&mut remaining)
            .strip_prefix("## ")
            .ok_or_else(|| error("memory_format", "Unexpected content outside a memory entry"))?
            .to_owned();
        let entry_header: EntryHeader = read_metadata(line(&mut remaining), "entry")?;
        let mut facts = Vec::new();
        loop {
            skip_blank(&mut remaining);
            if remaining.starts_with("<!-- /saya-entry -->") {
                if line(&mut remaining) != "<!-- /saya-entry -->" {
                    return Err(error(
                        "memory_format",
                        "Memory entry closing marker is invalid",
                    ));
                }
                break;
            }
            if facts.len() >= limits.max_facts {
                return Err(error("memory_limit", "Memory fact limit exceeded"));
            }
            let fact: FactHeader = read_metadata(line(&mut remaining), "fact")?;
            let marker = "\n<!-- /saya-fact -->";
            let end = remaining
                .find(marker)
                .ok_or_else(|| error("memory_format", "Memory fact closing marker is missing"))?;
            let body = remaining[..end]
                .strip_suffix('\r')
                .unwrap_or(&remaining[..end])
                .to_owned();
            remaining = &remaining[end + 1..];
            if line(&mut remaining) != "<!-- /saya-fact -->" {
                return Err(error(
                    "memory_format",
                    "Memory fact closing marker is invalid",
                ));
            }
            facts.push(MemoryFact {
                id: fact.id,
                kind: fact.kind,
                text: body,
                sources: fact.sources,
                aliases: fact.aliases,
                derived_from: fact.derived_from,
            });
        }
        document.entries.push(MemoryEntry {
            id: entry_header.id,
            title,
            scope: entry_header.scope,
            updated_at: entry_header.updated_at,
            facts,
            resources: entry_header.resources,
        });
        skip_blank(&mut remaining);
    }
    validate(&document, limits, secrets)?;
    Ok(document)
}

pub(super) fn render(
    document: &MemoryDocument,
    limits: MemoryLimits,
    secrets: &[String],
) -> Result<String, BoundaryError> {
    validate(document, limits, secrets)?;
    let header = Header {
        schema_version: document.schema_version,
        sources: document.sources.clone(),
        tombstones: document.tombstones.clone(),
        retractions: document.retractions.clone(),
    };
    let mut text = format!("# Saya Memory\n\n{}", metadata("memory", &header)?);
    for entry in &document.entries {
        text.push_str(&format!("\n## {}\n", entry.title));
        text.push_str(&metadata(
            "entry",
            &EntryHeader {
                id: entry.id.clone(),
                scope: entry.scope.clone(),
                updated_at: entry.updated_at.clone(),
                resources: entry.resources.clone(),
            },
        )?);
        for fact in &entry.facts {
            text.push_str(&metadata(
                "fact",
                &FactHeader {
                    id: fact.id.clone(),
                    kind: fact.kind,
                    sources: fact.sources.clone(),
                    aliases: fact.aliases.clone(),
                    derived_from: fact.derived_from.clone(),
                },
            )?);
            text.push_str(&fact.text);
            text.push_str("\n<!-- /saya-fact -->\n");
        }
        text.push_str("<!-- /saya-entry -->\n");
        if text.len() > limits.max_index_bytes {
            return Err(error("memory_limit", "Memory index exceeds its byte limit"));
        }
    }
    if text.len() > limits.max_index_bytes {
        return Err(error("memory_limit", "Memory index exceeds its byte limit"));
    }
    Ok(text)
}

fn short_text(value: &str, max: usize, empty: bool) -> bool {
    (empty || !value.trim().is_empty())
        && value.len() <= max
        && !value.chars().any(char::is_control)
}

pub(super) fn validate(
    document: &MemoryDocument,
    limits: MemoryLimits,
    secrets: &[String],
) -> Result<(), BoundaryError> {
    if document.schema_version != 1 {
        return Err(error(
            "memory_version",
            "Memory schema version is unsupported",
        ));
    }
    if document.entries.len() > limits.max_entries
        || document.sources.len() > limits.max_sources
        || document.tombstones.len() > limits.max_facts.saturating_mul(4)
        || document.retractions.len() > limits.max_sources.saturating_mul(4)
    {
        return Err(error("memory_limit", "Memory metadata limits exceeded"));
    }
    let mut sources = BTreeMap::new();
    for source in &document.sources {
        source.scope.validate()?;
        if !valid_id(&source.id, "src_")
            || !valid_digest(&source.revision)
            || !short_text(&source.reference, 512, false)
            || !short_text(&source.observed_at, 64, false)
            || !short_text(&source.description, 512, true)
            || (source.metadata_withheld
                && (source.reference != "withheld"
                    || source.observed_at != "withheld"
                    || !source.description.is_empty()))
            || sources.insert(source.id.as_str(), source).is_some()
        {
            return Err(error(
                "memory_source",
                "Memory source identity or metadata is invalid",
            ));
        }
    }
    let mut entries = BTreeSet::new();
    let mut facts = BTreeMap::new();
    let mut resources = BTreeSet::new();
    for entry in &document.entries {
        entry.scope.validate()?;
        if !valid_id(&entry.id, "mem_")
            || !entries.insert(&entry.id)
            || !short_text(&entry.title, 256, false)
            || entry.title.contains("<!--")
            || !short_text(&entry.updated_at, 64, false)
            || entry.facts.is_empty()
            || entry.resources.len() > limits.max_resources
        {
            return Err(error(
                "memory_entry",
                "Memory entry identity or metadata is invalid",
            ));
        }
        let mut entry_resources = BTreeSet::new();
        for path in &entry.resources {
            disk::validate_resource_path(path)?;
            if !entry_resources.insert(path) {
                return Err(error("memory_entry", "Memory resource is repeated"));
            }
            resources.insert(path);
        }
        for fact in &entry.facts {
            if !valid_id(&fact.id, "fact_")
                || facts
                    .insert(fact.id.as_str(), (fact, &entry.scope))
                    .is_some()
                || fact.text.trim().is_empty()
                || fact.text.len() > limits.max_fact_bytes
                || fact.text.contains("<!-- saya-")
                || fact.text.contains("<!-- /saya-")
                || fact.text.contains('\0')
                || fact.sources.is_empty()
                || fact.sources.len() > 32
                || fact.aliases.len() > 32
                || fact.derived_from.len() > 32
                || fact
                    .aliases
                    .iter()
                    .any(|alias| !short_text(alias, 256, false))
            {
                return Err(error(
                    "memory_fact",
                    "Memory fact identity or content is invalid",
                ));
            }
            let mut refs = BTreeSet::new();
            for reference in &fact.sources {
                let source = sources.get(reference.source_id.as_str()).ok_or_else(|| {
                    error("memory_source", "Memory fact references an unknown source")
                })?;
                if source.revision != reference.revision
                    || !source.scope.supports(&entry.scope)
                    || !refs.insert(&reference.source_id)
                {
                    return Err(error(
                        "memory_source",
                        "Memory source version or scope does not match its fact",
                    ));
                }
            }
        }
    }
    if facts.len() > limits.max_facts || resources.len() > limits.max_resources {
        return Err(error(
            "memory_limit",
            "Memory fact or resource limit exceeded",
        ));
    }
    let mut indegree = BTreeMap::<&str, usize>::new();
    let mut children = BTreeMap::<&str, Vec<&str>>::new();
    for (&id, (fact, scope)) in &facts {
        indegree.insert(id, fact.derived_from.len());
        let mut dependencies = BTreeSet::new();
        for parent in &fact.derived_from {
            let (_, parent_scope) = facts.get(parent.as_str()).ok_or_else(|| {
                error(
                    "memory_fact",
                    "Memory derivation references an unknown fact",
                )
            })?;
            if !parent_scope.supports(scope) || !dependencies.insert(parent) {
                return Err(error("memory_scope", "Memory derivation scope is invalid"));
            }
            children.entry(parent).or_default().push(id);
        }
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(&id, &degree)| (degree == 0).then_some(id))
        .collect::<VecDeque<_>>();
    let mut visited = 0;
    while let Some(id) = ready.pop_front() {
        visited += 1;
        for child in children.get(id).into_iter().flatten() {
            if let Some(degree) = indegree.get_mut(child) {
                *degree -= 1;
                if *degree == 0 {
                    ready.push_back(child);
                }
            }
        }
    }
    if visited != facts.len() {
        return Err(error("memory_fact", "Memory derivation contains a cycle"));
    }
    let mut operations = BTreeSet::new();
    for tombstone in &document.tombstones {
        tombstone.scope.validate()?;
        if !valid_id(&tombstone.operation_id, "op_")
            || !operations.insert((&tombstone.operation_id, &tombstone.scope))
            || tombstone.fact_ids.len() > limits.max_facts
            || tombstone.fingerprints.len() > limits.max_facts.saturating_mul(33)
            || tombstone.source_ids.len() > limits.max_sources
            || tombstone.resources.len() > limits.max_resources
            || tombstone.fact_ids.iter().any(|id| !valid_id(id, "fact_"))
            || tombstone
                .fingerprints
                .iter()
                .any(|value| !valid_digest(value))
        {
            return Err(error(
                "memory_format",
                "Memory tombstone metadata is invalid",
            ));
        }
        if tombstone.source_ids.iter().any(|id| !valid_id(id, "src_")) {
            return Err(error(
                "memory_source",
                "Memory tombstone source identity is invalid",
            ));
        }
        for path in &tombstone.resources {
            disk::validate_resource_path(path)?;
        }
        validate_entry_metadata_suppressions(&tombstone.entry_metadata, limits)?;
    }
    let mut retracted_sources = BTreeSet::new();
    for retraction in &document.retractions {
        if !valid_id(&retraction.source_id, "src_") || !valid_digest(&retraction.revision) {
            return Err(error(
                "memory_source",
                "Memory source retraction is invalid",
            ));
        }
        retraction.scope.validate()?;
        if !retracted_sources.insert((&retraction.source_id, &retraction.revision))
            || sources
                .get(retraction.source_id.as_str())
                .is_some_and(|source| source.scope != retraction.scope)
        {
            return Err(error(
                "memory_source",
                "Memory retraction source scope or identity is invalid",
            ));
        }
        if retraction.resources.len() > limits.max_resources {
            return Err(error(
                "memory_limit",
                "Memory retraction resource limit exceeded",
            ));
        }
        for path in &retraction.resources {
            disk::validate_resource_path(path)?;
        }
        validate_entry_metadata_suppressions(&retraction.entry_metadata, limits)?;
    }
    let encoded = encode_bounded(document, state::max_state_bytes(limits))?;
    let encoded = std::str::from_utf8(&encoded)
        .map_err(|_| error("memory_format", "Memory document encoding is invalid"))?;
    if crate::redaction::contains_secret(encoded, secrets) {
        return Err(error(
            "sensitive_data",
            "Memory document contains protected data",
        ));
    }
    Ok(())
}

/// Applies current forget and source-withdrawal constraints to an already
/// validated document. Derived facts disappear when any required parent does.
pub(super) fn active_document(document: &MemoryDocument) -> MemoryDocument {
    let mut active = document.clone();
    let mut removed = BTreeSet::<String>::new();
    let mut forgotten = BTreeMap::<&MemoryScope, (BTreeSet<&str>, BTreeSet<&str>)>::new();
    for stone in &document.tombstones {
        let targets = forgotten.entry(&stone.scope).or_default();
        targets.0.extend(stone.fact_ids.iter().map(String::as_str));
        targets
            .1
            .extend(stone.fingerprints.iter().map(String::as_str));
    }
    let retracted = document
        .retractions
        .iter()
        .map(|source| (source.source_id.as_str(), source.revision.as_str()))
        .collect::<BTreeSet<_>>();
    let mut children = BTreeMap::<String, Vec<String>>::new();
    for entry in &document.entries {
        for fact in &entry.facts {
            for parent in &fact.derived_from {
                children
                    .entry(parent.clone())
                    .or_default()
                    .push(fact.id.clone());
            }
        }
    }
    for entry in &mut active.entries {
        let before = entry.facts.len();
        entry.facts.retain_mut(|fact| {
            let forgotten = forgotten.get(&entry.scope).is_some_and(|targets| {
                targets.0.contains(fact.id.as_str())
                    || targets.1.contains(fact_fingerprint(&fact.text).as_str())
                    || fact
                        .aliases
                        .iter()
                        .any(|alias| targets.1.contains(fact_fingerprint(alias).as_str()))
            });
            fact.sources.retain(|source| {
                !retracted.contains(&(source.source_id.as_str(), source.revision.as_str()))
            });
            let keep = !forgotten && !fact.sources.is_empty();
            if !keep {
                removed.insert(fact.id.clone());
            }
            keep
        });
        if entry.facts.len() != before {
            entry.resources.clear();
        }
    }
    let mut pending = removed.iter().cloned().collect::<VecDeque<_>>();
    while let Some(id) = pending.pop_front() {
        for child in children.get(&id).into_iter().flatten() {
            if removed.insert(child.clone()) {
                pending.push_back(child.clone());
            }
        }
    }
    for entry in &mut active.entries {
        let before = entry.facts.len();
        entry.facts.retain(|fact| !removed.contains(&fact.id));
        if entry.facts.len() != before {
            entry.resources.clear();
        }
    }
    let mut affected_sources = document
        .tombstones
        .iter()
        .flat_map(|stone| stone.source_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    for fact in document
        .entries
        .iter()
        .flat_map(|entry| &entry.facts)
        .filter(|fact| removed.contains(&fact.id))
    {
        affected_sources.extend(fact.sources.iter().map(|source| source.source_id.clone()));
    }
    for source in &mut active.sources {
        if affected_sources.contains(&source.id) {
            source.reference = "withheld".into();
            source.observed_at = "withheld".into();
            source.description.clear();
            source.metadata_withheld = true;
        }
    }
    for entry in &mut active.entries {
        let title_fingerprint = fact_fingerprint(&entry.title);
        let updated_at_fingerprint = fact_fingerprint(&entry.updated_at);
        let mut title_suppressed = false;
        let mut updated_at_suppressed = false;
        for suppression in document
            .tombstones
            .iter()
            .filter(|stone| stone.scope == entry.scope)
            .flat_map(|stone| &stone.entry_metadata)
            .chain(
                document
                    .retractions
                    .iter()
                    .filter(|source| source.scope.supports(&entry.scope))
                    .flat_map(|source| &source.entry_metadata),
            )
            .filter(|suppression| suppression.entry_id == entry.id)
        {
            title_suppressed |= suppression.title_fingerprint == title_fingerprint;
            updated_at_suppressed |=
                suppression.updated_at_fingerprint.as_ref() == Some(&updated_at_fingerprint);
        }
        if title_suppressed {
            entry.title = "Memory entry".into();
        }
        if updated_at_suppressed {
            entry.updated_at = "withheld".into();
        }
        entry.resources.retain(|path| {
            !document
                .tombstones
                .iter()
                .any(|stone| stone.scope == entry.scope && stone.resources.contains(path))
                && !document.retractions.iter().any(|source| {
                    source.scope.supports(&entry.scope) && source.resources.contains(path)
                })
        });
    }
    active.entries.retain(|entry| !entry.facts.is_empty());
    active
}

fn validate_entry_metadata_suppressions(
    values: &[MemoryEntryMetadataSuppression],
    limits: MemoryLimits,
) -> Result<(), BoundaryError> {
    if values.len() > limits.max_entries
        || values.iter().any(|value| {
            !valid_id(&value.entry_id, "mem_")
                || !valid_digest(&value.title_fingerprint)
                || value
                    .updated_at_fingerprint
                    .as_ref()
                    .is_some_and(|value| !valid_digest(value))
        })
    {
        return Err(error(
            "memory_format",
            "Memory title suppression metadata is invalid",
        ));
    }
    Ok(())
}
