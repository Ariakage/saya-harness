use super::*;
use std::collections::BTreeSet;

const MAX_PAGE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryQueryLimits {
    pub max_page_bytes: usize,
    pub max_results: usize,
    pub max_query_bytes: usize,
    pub max_projection_bytes: usize,
    pub max_routing_bytes: usize,
    pub max_selected_bytes: usize,
}
impl Default for MemoryQueryLimits {
    fn default() -> Self {
        Self {
            max_page_bytes: 16 * 1024,
            max_results: 64,
            max_query_bytes: 1024,
            max_projection_bytes: 16 * 1024,
            max_routing_bytes: 4096,
            max_selected_bytes: 8192,
        }
    }
}
impl MemoryQueryLimits {
    pub fn validate(&self) -> Result<(), BoundaryError> {
        if self.max_page_bytes < 512
            || self.max_page_bytes > MAX_PAGE_BYTES
            || self.max_results == 0
            || self.max_results > 200
            || self.max_query_bytes == 0
            || self.max_query_bytes > 4096
            || self.max_projection_bytes > MAX_PAGE_BYTES
            || self.max_routing_bytes > MAX_PAGE_BYTES
            || self.max_selected_bytes > MAX_PAGE_BYTES
        {
            return Err(error("memory_config", "Memory query limits are invalid"));
        }
        Ok(())
    }
}

/// Positions address the current normalized query result and original UTF-8
/// fact bytes. A cursor cannot revive a previous snapshot or a different query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryCursor {
    pub revision: String,
    pub query_sha256: String,
    pub record_index: usize,
    pub byte_offset: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryReadOptions {
    pub entry_id: Option<String>,
    pub fact_id: Option<String>,
    pub cursor: Option<MemoryCursor>,
    pub max_bytes: usize,
    pub max_items: usize,
}
impl Default for MemoryReadOptions {
    fn default() -> Self {
        Self {
            entry_id: None,
            fact_id: None,
            cursor: None,
            max_bytes: 16 * 1024,
            max_items: 16,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemorySearchOptions {
    pub query: String,
    pub cursor: Option<MemoryCursor>,
    pub max_bytes: usize,
    pub max_items: usize,
}
impl Default for MemorySearchOptions {
    fn default() -> Self {
        Self {
            query: String::new(),
            cursor: None,
            max_bytes: 16 * 1024,
            max_items: 16,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryReference {
    pub entry_id: String,
    pub fact_id: String,
    pub scope: MemoryScope,
    pub snapshot_revision: String,
    pub fact_revision: String,
    pub sources: Vec<MemorySourceRef>,
    pub source_metadata_withheld: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryFragment {
    pub reference: MemoryReference,
    pub title: String,
    pub kind: MemoryKind,
    pub text: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub total_bytes: usize,
    pub complete: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryReadPage {
    pub revision: String,
    pub forget_revision: String,
    pub items: Vec<MemoryFragment>,
    pub next_cursor: Option<MemoryCursor>,
    pub truncated: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryMatchField {
    Title,
    Text,
    Alias,
    Source,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySearchHit {
    pub fact: MemoryFragment,
    pub score: u32,
    pub matched_fields: Vec<MemoryMatchField>,
    pub matched_aliases: Vec<String>,
    pub aliases_omitted: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemorySearchPage {
    pub revision: String,
    pub forget_revision: String,
    pub matches: Vec<MemorySearchHit>,
    pub next_cursor: Option<MemoryCursor>,
    pub truncated: bool,
}

pub(super) struct Query {
    pub(super) text: String,
    terms: Vec<String>,
}
pub(super) struct RankedFact<'a> {
    pub(super) entry: &'a MemoryEntry,
    pub(super) fact: &'a MemoryFact,
    pub(super) score: u32,
    fields: Vec<MemoryMatchField>,
    aliases: Vec<String>,
    alias_count: usize,
}
fn normalized(text: &str) -> String {
    text.nfc().flat_map(char::to_lowercase).collect()
}
pub(super) fn prepare_query(
    store: &GlobalMemoryStore,
    text: &str,
    limits: MemoryQueryLimits,
    empty: bool,
) -> Result<Query, BoundaryError> {
    limits.validate()?;
    if text.len() > limits.max_query_bytes
        || text
            .chars()
            .any(|character| character.is_control() && !character.is_whitespace())
    {
        return Err(error("memory_args", "Memory query exceeds its text limits"));
    }
    if crate::redaction::contains_secret(text, &store.secrets.snapshot()?) {
        return Err(error(
            "sensitive_data",
            "Memory query contains protected data",
        ));
    }
    let text = normalized(text.trim());
    if text.is_empty() && !empty {
        return Err(error(
            "memory_args",
            "Memory search requires a nonempty query",
        ));
    }
    let terms = text
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|term| term.chars().count() >= 2)
        .take(32)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(Query { text, terms })
}
fn field_score(text: &str, query: &Query, exact: u32, term: u32) -> u32 {
    if query.text.is_empty() {
        return 0;
    }
    let text = normalized(text);
    if text.contains(&query.text) {
        return exact;
    }
    query
        .terms
        .iter()
        .filter(|part| text.contains(part.as_str()))
        .take(3)
        .count() as u32
        * term
}
pub(super) fn ranked<'a>(
    snapshot: &'a MemorySnapshot,
    query: &Query,
    preference_fallback: bool,
    cancel: &AtomicBool,
) -> Result<Vec<RankedFact<'a>>, BoundaryError> {
    let source_text = snapshot
        .sources
        .iter()
        .map(|source| {
            let text = if source.metadata_withheld {
                source.id.clone()
            } else {
                format!("{} {} {}", source.id, source.reference, source.description)
            };
            (source.id.as_str(), text)
        })
        .collect::<BTreeMap<_, _>>();
    let mut matches = Vec::new();
    for entry in &snapshot.entries {
        check_cancelled(cancel)?;
        let title_score = field_score(&entry.title, query, 60, 10);
        for fact in &entry.facts {
            check_cancelled(cancel)?;
            let text_score = field_score(&fact.text, query, 100, 20);
            let aliases = fact
                .aliases
                .iter()
                .filter(|alias| field_score(alias, query, 120, 25) > 0)
                .cloned()
                .collect::<Vec<_>>();
            let source_score = fact
                .sources
                .iter()
                .filter_map(|reference| source_text.get(reference.source_id.as_str()))
                .map(|text| field_score(text, query, 30, 5))
                .max()
                .unwrap_or(0);
            let alias_score = aliases
                .iter()
                .map(|alias| field_score(alias, query, 120, 25))
                .max()
                .unwrap_or(0);
            let mut score = title_score + text_score + alias_score + source_score;
            let mut fields = Vec::new();
            if title_score > 0 {
                fields.push(MemoryMatchField::Title);
            }
            if text_score > 0 {
                fields.push(MemoryMatchField::Text);
            }
            if !aliases.is_empty() {
                fields.push(MemoryMatchField::Alias);
            }
            if source_score > 0 {
                fields.push(MemoryMatchField::Source);
            }
            if score == 0
                && preference_fallback
                && entry.scope == MemoryScope::Global
                && fact.kind == MemoryKind::ExplicitPreference
            {
                score = 1;
            }
            if score == 0 {
                continue;
            }
            let alias_count = aliases.len();
            matches.push(RankedFact {
                entry,
                fact,
                score,
                fields,
                aliases: aliases.into_iter().take(4).collect(),
                alias_count,
            });
        }
    }
    matches.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.entry.id.cmp(&right.entry.id))
            .then_with(|| left.fact.id.cmp(&right.fact.id))
    });
    Ok(matches)
}

pub(super) fn fragment(
    snapshot: &MemorySnapshot,
    entry: &MemoryEntry,
    fact: &MemoryFact,
    start: usize,
) -> Result<MemoryFragment, BoundaryError> {
    let source_metadata_withheld = fact.sources.iter().any(|reference| {
        snapshot
            .sources
            .iter()
            .any(|source| source.id == reference.source_id && source.metadata_withheld)
    });
    Ok(MemoryFragment {
        reference: MemoryReference {
            entry_id: entry.id.clone(),
            fact_id: fact.id.clone(),
            scope: entry.scope.clone(),
            snapshot_revision: snapshot.revision.clone(),
            fact_revision: digest(&encode_bounded(fact, 32 * 1024 * 1024)?),
            sources: fact.sources.clone(),
            source_metadata_withheld,
        },
        title: entry.title.clone(),
        kind: fact.kind,
        text: String::new(),
        start_byte: start,
        end_byte: start,
        total_bytes: fact.text.len(),
        complete: false,
    })
}
pub(super) fn prefix_end(text: &str, start: usize, encoded_budget: usize) -> usize {
    let mut spent = 0usize;
    let mut end = start;
    for character in text[start..].chars() {
        let bytes = match character {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            character if character <= '\u{1f}' => 6,
            character => character.len_utf8(),
        };
        if spent.saturating_add(bytes) > encoded_budget {
            break;
        }
        spent += bytes;
        end += character.len_utf8();
    }
    end
}
fn query_digest<T: Serialize>(
    store: &GlobalMemoryStore,
    kind: &str,
    value: &T,
    limits: MemoryQueryLimits,
) -> Result<String, BoundaryError> {
    Ok(digest(&encode_bounded(
        &("saya-memory-query-v1", store.binding(), kind, value, limits),
        MAX_PAGE_BYTES,
    )?))
}
fn cursor(
    snapshot: &MemorySnapshot,
    query: &str,
    record_index: usize,
    byte_offset: usize,
) -> MemoryCursor {
    MemoryCursor {
        revision: snapshot.revision.clone(),
        query_sha256: query.into(),
        record_index,
        byte_offset,
    }
}
fn validate_cursor(
    value: Option<&MemoryCursor>,
    snapshot: &MemorySnapshot,
    query: &str,
) -> Result<(), BoundaryError> {
    let Some(value) = value else {
        return Ok(());
    };
    if !valid_digest(&value.revision) || !valid_digest(&value.query_sha256) {
        return Err(error(
            "memory_cursor_invalid",
            "Memory cursor identity is invalid",
        ));
    }
    if value.revision != snapshot.revision {
        return Err(error(
            "memory_cursor_stale",
            "Memory changed since this cursor was issued",
        ));
    }
    if value.query_sha256 != query {
        return Err(error(
            "memory_cursor_invalid",
            "Memory cursor belongs to another query",
        ));
    }
    Ok(())
}
fn page_limits(
    max_bytes: usize,
    max_items: usize,
    limits: MemoryQueryLimits,
) -> Result<(usize, usize), BoundaryError> {
    limits.validate()?;
    if max_bytes == 0 || max_items == 0 {
        return Err(error("memory_args", "Memory page bounds must be positive"));
    }
    Ok((
        max_bytes.min(limits.max_page_bytes),
        max_items.min(limits.max_results),
    ))
}
fn query_limit() -> BoundaryError {
    error(
        "memory_query_limit",
        "Memory record metadata exceeds the page budget",
    )
}

impl GlobalMemoryStore {
    /// Pages current visible fact bodies. Requested bounds are clamped to the
    /// host limits. Mandatory provenance is never silently removed to fit.
    pub fn read_page(
        &self,
        options: MemoryReadOptions,
        limits: MemoryQueryLimits,
    ) -> Result<MemoryReadPage, BoundaryError> {
        self.read_page_cancellable(options, limits, &AtomicBool::new(false))
    }
    pub fn read_page_cancellable(
        &self,
        options: MemoryReadOptions,
        limits: MemoryQueryLimits,
        cancel: &AtomicBool,
    ) -> Result<MemoryReadPage, BoundaryError> {
        let (max_bytes, max_items) = page_limits(options.max_bytes, options.max_items, limits)?;
        if options
            .entry_id
            .as_ref()
            .is_some_and(|id| !valid_id(id, "mem_"))
            || options
                .fact_id
                .as_ref()
                .is_some_and(|id| !valid_id(id, "fact_"))
        {
            return Err(error("memory_args", "Memory read selector is invalid"));
        }
        let snapshot = self.snapshot_cancellable(cancel)?;
        let query = query_digest(self, "read", &(&options.entry_id, &options.fact_id), limits)?;
        validate_cursor(options.cursor.as_ref(), &snapshot, &query)?;
        let records = snapshot
            .entries
            .iter()
            .filter(|entry| options.entry_id.as_ref().is_none_or(|id| id == &entry.id))
            .flat_map(|entry| {
                entry
                    .facts
                    .iter()
                    .filter(|fact| options.fact_id.as_ref().is_none_or(|id| id == &fact.id))
                    .map(move |fact| (entry, fact))
            })
            .collect::<Vec<_>>();
        if records.is_empty() && (options.entry_id.is_some() || options.fact_id.is_some()) {
            return Err(error(
                "memory_not_found",
                "Memory read target is not active in this workspace",
            ));
        }
        let mut index = options
            .cursor
            .as_ref()
            .map_or(0, |cursor| cursor.record_index);
        let mut offset = options
            .cursor
            .as_ref()
            .map_or(0, |cursor| cursor.byte_offset);
        if options.cursor.is_some()
            && (index >= records.len()
                || offset >= records[index].1.text.len()
                || !records[index].1.text.is_char_boundary(offset))
        {
            return Err(error(
                "memory_cursor_invalid",
                "Memory cursor position is invalid",
            ));
        }
        let mut page = MemoryReadPage {
            revision: snapshot.revision.clone(),
            forget_revision: snapshot.forget_revision.clone(),
            items: Vec::new(),
            next_cursor: None,
            truncated: false,
        };
        while index < records.len() && page.items.len() < max_items {
            check_cancelled(cancel)?;
            let (entry, fact) = records[index];
            let item = fragment(&snapshot, entry, fact, offset)?;
            let mut probe = page.clone();
            probe.items.push(item.clone());
            probe.next_cursor = Some(cursor(&snapshot, &query, usize::MAX, usize::MAX));
            let metadata_bytes = encode_bounded(&probe, 2 * MAX_PAGE_BYTES)?.len();
            if metadata_bytes >= max_bytes {
                if page.items.is_empty() {
                    return Err(query_limit());
                }
                break;
            }
            let end = prefix_end(&fact.text, offset, max_bytes - metadata_bytes);
            if end == offset {
                if page.items.is_empty() {
                    return Err(query_limit());
                }
                break;
            }
            let mut item = item;
            item.text = fact.text[offset..end].into();
            item.end_byte = end;
            item.complete = offset == 0 && end == fact.text.len();
            page.items.push(item);
            if end < fact.text.len() {
                offset = end;
                break;
            }
            index += 1;
            offset = 0;
        }
        if index < records.len() {
            page.next_cursor = Some(cursor(&snapshot, &query, index, offset));
        }
        page.truncated = page.next_cursor.is_some() || page.items.iter().any(|item| !item.complete);
        let _ = encode_bounded(&page, max_bytes).map_err(|_| query_limit())?;
        check_cancelled(cancel)?;
        Ok(page)
    }
    pub fn search(
        &self,
        options: MemorySearchOptions,
        limits: MemoryQueryLimits,
    ) -> Result<MemorySearchPage, BoundaryError> {
        self.search_cancellable(options, limits, &AtomicBool::new(false))
    }
    pub fn search_cancellable(
        &self,
        options: MemorySearchOptions,
        limits: MemoryQueryLimits,
        cancel: &AtomicBool,
    ) -> Result<MemorySearchPage, BoundaryError> {
        let (max_bytes, max_items) = page_limits(options.max_bytes, options.max_items, limits)?;
        let prepared = prepare_query(self, &options.query, limits, false)?;
        let snapshot = self.snapshot_cancellable(cancel)?;
        let query = query_digest(self, "search", &prepared.text, limits)?;
        validate_cursor(options.cursor.as_ref(), &snapshot, &query)?;
        let matches = ranked(&snapshot, &prepared, false, cancel)?;
        let mut index = options
            .cursor
            .as_ref()
            .map_or(0, |cursor| cursor.record_index);
        if options
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.byte_offset != 0 || index >= matches.len())
        {
            return Err(error(
                "memory_cursor_invalid",
                "Memory search cursor position is invalid",
            ));
        }
        let mut page = MemorySearchPage {
            revision: snapshot.revision.clone(),
            forget_revision: snapshot.forget_revision.clone(),
            matches: Vec::new(),
            next_cursor: None,
            truncated: false,
        };
        while index < matches.len() && page.matches.len() < max_items {
            check_cancelled(cancel)?;
            let ranked = &matches[index];
            let start = excerpt_start(&ranked.fact.text, &prepared, cancel)?;
            let hit = MemorySearchHit {
                fact: fragment(&snapshot, ranked.entry, ranked.fact, start)?,
                score: ranked.score,
                matched_fields: ranked.fields.clone(),
                matched_aliases: ranked.aliases.clone(),
                aliases_omitted: ranked.alias_count.saturating_sub(ranked.aliases.len()),
            };
            let mut probe = page.clone();
            probe.matches.push(hit.clone());
            probe.next_cursor = Some(cursor(&snapshot, &query, usize::MAX, 0));
            let metadata_bytes = encode_bounded(&probe, 2 * MAX_PAGE_BYTES)?.len();
            if metadata_bytes >= max_bytes {
                if page.matches.is_empty() {
                    return Err(query_limit());
                }
                break;
            }
            let end = prefix_end(
                &ranked.fact.text,
                start,
                (max_bytes - metadata_bytes).min(2048),
            );
            if end == start {
                if page.matches.is_empty() {
                    return Err(query_limit());
                }
                break;
            }
            let mut hit = hit;
            hit.fact.text = ranked.fact.text[start..end].into();
            hit.fact.end_byte = end;
            hit.fact.complete = start == 0 && end == ranked.fact.text.len();
            page.matches.push(hit);
            index += 1;
        }
        if index < matches.len() {
            page.next_cursor = Some(cursor(&snapshot, &query, index, 0));
        }
        page.truncated = page.next_cursor.is_some()
            || page
                .matches
                .iter()
                .any(|hit| !hit.fact.complete || hit.aliases_omitted > 0);
        let _ = encode_bounded(&page, max_bytes).map_err(|_| query_limit())?;
        check_cancelled(cancel)?;
        Ok(page)
    }
}

// The excerpt offset addresses original bytes, never a normalized byte index.
// Locate a matching line first. Long-line centering is a bounded display hint.
pub(super) fn excerpt_start(
    text: &str,
    query: &Query,
    cancel: &AtomicBool,
) -> Result<usize, BoundaryError> {
    let mut line_offset = 0;
    for line in text.split_inclusive('\n') {
        check_cancelled(cancel)?;
        if field_score(line, query, 1, 1) > 0 {
            let normalized = normalized(line);
            let found = normalized
                .find(&query.text)
                .or_else(|| {
                    query
                        .terms
                        .iter()
                        .filter_map(|term| normalized.find(term))
                        .min()
                })
                .unwrap_or(0);
            let original = if line.is_ascii() {
                found
            } else {
                let boundaries = line
                    .char_indices()
                    .map(|(index, _)| index)
                    .chain(std::iter::once(line.len()))
                    .collect::<Vec<_>>();
                let position = boundaries.partition_point(|&index| {
                    if cancel.load(Ordering::Acquire) {
                        return false;
                    }
                    line[..index]
                        .nfc()
                        .flat_map(char::to_lowercase)
                        .map(char::len_utf8)
                        .sum::<usize>()
                        < found
                });
                check_cancelled(cancel)?;
                boundaries.get(position).copied().unwrap_or(line.len())
            };
            let mut start = original.saturating_sub(256).min(line.len());
            while !line.is_char_boundary(start) {
                start -= 1;
            }
            return Ok(line_offset + start);
        }
        line_offset += line.len();
    }
    Ok(0)
}
