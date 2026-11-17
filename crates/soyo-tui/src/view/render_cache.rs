//! A bounded cache of recent assistant document projections.
use crate::rich_text::{RenderedDocument, render_document};
use std::{
    borrow::Cow,
    collections::{HashSet, VecDeque, hash_map::DefaultHasher},
    fmt,
    hash::{Hash, Hasher},
    sync::Arc,
};

pub(super) const MAX_ENTRIES: usize = 16;
const MAX_LINES: usize = 10_000;
const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_SPANS: usize = 65_536;
const MAX_ANCHORS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    message: u64,
    width: usize,
    fingerprint: u64,
    source_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct Cost {
    lines: usize,
    bytes: usize,
    spans: usize,
    anchors: usize,
}

impl Cost {
    fn of(document: &RenderedDocument) -> Self {
        let mut cost = Self {
            lines: document.lines.len(),
            anchors: document.footnotes.len(),
            ..Self::default()
        };
        for line in &document.lines {
            cost.spans = cost.spans.saturating_add(line.spans.len());
            for span in &line.spans {
                let allocated = match &span.content {
                    Cow::Owned(text) => text.capacity(),
                    Cow::Borrowed(text) => text.len(),
                };
                cost.bytes = cost.bytes.saturating_add(allocated);
            }
        }
        for anchor in &document.footnotes {
            cost.bytes = cost.bytes.saturating_add(anchor.key.capacity());
        }
        cost
    }

    fn plus(self, other: Self) -> Self {
        Self {
            lines: self.lines.saturating_add(other.lines),
            bytes: self.bytes.saturating_add(other.bytes),
            spans: self.spans.saturating_add(other.spans),
            anchors: self.anchors.saturating_add(other.anchors),
        }
    }

    fn fits(self) -> bool {
        self.lines <= MAX_LINES
            && self.bytes <= MAX_BYTES
            && self.spans <= MAX_SPANS
            && self.anchors <= MAX_ANCHORS
    }
}

struct Entry {
    key: Key,
    cost: Cost,
    document: Arc<RenderedDocument>,
}

#[derive(Default)]
pub(super) struct RenderCache {
    entries: VecDeque<Entry>,
}

impl fmt::Debug for RenderCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderCache")
            .field("entries", &self.entries.len())
            .field("cost", &self.cost())
            .finish()
    }
}

impl RenderCache {
    pub(super) fn document(
        &mut self,
        message: u64,
        width: usize,
        source: &str,
        eligible: bool,
    ) -> Arc<RenderedDocument> {
        if !eligible {
            self.remove_message(message);
            return Arc::new(render_document(source, width));
        }
        let mut hasher = DefaultHasher::new();
        source.hash(&mut hasher);
        let key = Key {
            message,
            width,
            fingerprint: hasher.finish(),
            source_bytes: source.len(),
        };
        if let Some(index) = self.entries.iter().position(|entry| entry.key == key)
            && let Some(entry) = self.entries.remove(index)
        {
            let document = Arc::clone(&entry.document);
            self.entries.push_back(entry);
            return document;
        }
        self.remove_message(message);
        let document = Arc::new(render_document(source, width));
        self.admit(key, Arc::clone(&document));
        document
    }

    fn cost(&self) -> Cost {
        self.entries
            .iter()
            .fold(Cost::default(), |total, entry| total.plus(entry.cost))
    }

    fn admit(&mut self, key: Key, document: Arc<RenderedDocument>) {
        let cost = Cost::of(&document);
        if !cost.fits() {
            return;
        }
        let mut reserved = cost;
        let mut reserved_entries = 1usize;
        for newer in self
            .entries
            .iter()
            .filter(|entry| entry.key.message > key.message)
        {
            reserved = reserved.plus(newer.cost);
            reserved_entries += 1;
        }
        // Reject before eviction when newer messages leave no room for this candidate.
        if reserved_entries > MAX_ENTRIES || !reserved.fits() {
            return;
        }
        while self.entries.len() >= MAX_ENTRIES || !self.cost().plus(cost).fits() {
            let Some((index, _)) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.key.message)
            else {
                return;
            };
            self.entries.remove(index);
        }
        self.entries.push_back(Entry {
            key,
            cost,
            document,
        });
    }

    pub(super) fn retain(&mut self, messages: &HashSet<u64>) {
        self.entries
            .retain(|entry| messages.contains(&entry.key.message));
    }

    pub(super) fn remove_message(&mut self, message: u64) {
        self.entries.retain(|entry| entry.key.message != message);
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rich_text::FootnoteAnchor;
    use ratatui::text::{Line, Span};

    #[test]
    fn identical_source_and_width_reuse_the_document_and_changes_replace_it() {
        let mut cache = RenderCache::default();
        let first = cache.document(1, 80, "**first**", true);
        let repeated = cache.document(1, 80, "**first**", true);
        assert!(Arc::ptr_eq(&first, &repeated));
        let changed = cache.document(1, 80, "**changed**", true);
        assert!(!Arc::ptr_eq(&first, &changed));
        assert_eq!(cache.entries.len(), 1);
        let resized = cache.document(1, 20, "**changed**", true);
        assert!(!Arc::ptr_eq(&changed, &resized));
        assert_eq!(cache.entries.len(), 1);
        cache.clear();
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn eligibility_and_recent_message_limits_bound_the_cache() {
        let mut cache = RenderCache::default();
        cache.document(0, 80, "old", false);
        assert!(cache.entries.is_empty());
        for id in 1..=100 {
            cache.document(id, 80, "message", true);
            assert!(cache.entries.len() <= MAX_ENTRIES);
            assert!(cache.cost().fits());
        }
        assert!(cache.entries.iter().all(|entry| entry.key.message >= 85));
        let first = Arc::clone(&cache.entries.back().unwrap().document);
        cache.retain(&HashSet::from([100]));
        assert_eq!(cache.entries.len(), 1);
        assert!(Arc::ptr_eq(
            &first,
            &cache.document(100, 80, "message", true)
        ));
        cache.remove_message(100);
        assert!(cache.entries.is_empty());
        cache.document(100, 80, "message", true);
        cache.document(100, 80, "message", false);
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn every_document_budget_is_checked_before_admission() {
        let key = Key {
            message: 1,
            width: 80,
            fingerprint: 1,
            source_bytes: 1,
        };
        let documents = [
            RenderedDocument {
                lines: vec![Line::raw("x".repeat(MAX_BYTES + 1))],
                footnotes: vec![],
            },
            RenderedDocument {
                lines: vec![Line::raw("x"); MAX_LINES + 1],
                footnotes: vec![],
            },
            RenderedDocument {
                lines: vec![Line::from(vec![Span::raw("x"); MAX_SPANS + 1])],
                footnotes: vec![],
            },
            RenderedDocument {
                lines: vec![],
                footnotes: (0..=MAX_ANCHORS)
                    .map(|index| FootnoteAnchor {
                        key: "key".into(),
                        number: 1,
                        line: 0,
                        column: 0,
                        width: 3,
                        definition: false,
                        occurrence: index,
                    })
                    .collect(),
            },
        ];
        for document in documents {
            let mut cache = RenderCache::default();
            cache.admit(key, Arc::new(document));
            assert!(cache.entries.is_empty());
        }
    }

    #[test]
    fn older_budget_candidates_do_not_evict_newer_documents_during_a_frame() {
        let mut cache = RenderCache::default();
        let newer = Arc::new(RenderedDocument {
            lines: vec![Line::raw("x".repeat(MAX_BYTES * 3 / 4))],
            footnotes: vec![],
        });
        cache.admit(
            Key {
                message: 10,
                width: 80,
                fingerprint: 10,
                source_bytes: 10,
            },
            Arc::clone(&newer),
        );
        let older = Arc::new(RenderedDocument {
            lines: vec![Line::raw("x".repeat(MAX_BYTES / 2))],
            footnotes: vec![],
        });
        cache.admit(
            Key {
                message: 9,
                width: 80,
                fingerprint: 9,
                source_bytes: 9,
            },
            older,
        );
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.entries[0].key.message, 10);
        assert!(Arc::ptr_eq(&cache.entries[0].document, &newer));
        assert!(cache.cost().fits());
    }

    #[test]
    fn rejected_admission_preserves_older_reusable_entries_without_partial_eviction() {
        let mut cache = RenderCache::default();
        let older = Arc::new(RenderedDocument {
            lines: vec![Line::raw("x".repeat(MAX_BYTES * 3 / 8))],
            footnotes: vec![],
        });
        let newer = Arc::new(RenderedDocument {
            lines: vec![Line::raw("y".repeat(MAX_BYTES * 3 / 8))],
            footnotes: vec![],
        });
        cache.admit(
            Key {
                message: 8,
                width: 80,
                fingerprint: 8,
                source_bytes: 8,
            },
            Arc::clone(&older),
        );
        cache.admit(
            Key {
                message: 10,
                width: 80,
                fingerprint: 10,
                source_bytes: 10,
            },
            Arc::clone(&newer),
        );
        let candidate = Arc::new(RenderedDocument {
            lines: vec![Line::raw("z".repeat(MAX_BYTES * 3 / 4))],
            footnotes: vec![],
        });
        cache.admit(
            Key {
                message: 9,
                width: 80,
                fingerprint: 9,
                source_bytes: 9,
            },
            candidate,
        );
        assert_eq!(cache.entries.len(), 2);
        assert!(
            cache
                .entries
                .iter()
                .any(|entry| entry.key.message == 8 && Arc::ptr_eq(&entry.document, &older))
        );
        assert!(
            cache
                .entries
                .iter()
                .any(|entry| entry.key.message == 10 && Arc::ptr_eq(&entry.document, &newer))
        );
        assert!(cache.cost().fits());
    }
}
