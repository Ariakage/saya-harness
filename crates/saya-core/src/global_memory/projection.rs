use super::query::{MemoryFragment, MemoryQueryLimits};
use super::*;

pub const MEMORY_CONTEXT_INSTRUCTIONS: &str = include_str!("../../prompts/v1/memory-context.md");
const PREFIX: &str = "# Memory context data\n";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryProjectionOptions {
    pub query: String,
    pub max_bytes: usize,
    pub routing_bytes: usize,
    pub body_bytes: usize,
    pub max_routing_entries: usize,
    pub max_selected_facts: usize,
}
impl Default for MemoryProjectionOptions {
    fn default() -> Self {
        Self {
            query: String::new(),
            max_bytes: 16 * 1024,
            routing_bytes: 4096,
            body_bytes: 8192,
            max_routing_entries: 16,
            max_selected_facts: 16,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRoute {
    pub entry_id: String,
    pub title: String,
    pub scope: MemoryScope,
    pub updated_at: String,
    pub fact_count: usize,
    pub resources: Vec<MemoryResourceInfo>,
    pub resources_omitted: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryProjectionStatus {
    Empty,
    Included,
    BudgetExhausted,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryProjection {
    pub status: MemoryProjectionStatus,
    pub revision: String,
    pub forget_revision: String,
    pub workspace: WorkspaceIdentity,
    pub routing: Vec<MemoryRoute>,
    pub selected: Vec<MemoryFragment>,
    pub routing_omitted: usize,
    pub facts_omitted: usize,
    pub truncated: bool,
    pub cache_key: String,
    /// Inject this bounded text once. Structured fields are host inspection
    /// metadata. They are not a second copy to append to a model request.
    pub text: String,
}
#[derive(Serialize)]
struct Data<'a> {
    schema_version: u32,
    revision: &'a str,
    forget_revision: &'a str,
    workspace: &'a WorkspaceIdentity,
    routing: &'a [MemoryRoute],
    selected: &'a [MemoryFragment],
    routing_omitted: usize,
    facts_omitted: usize,
}

impl GlobalMemoryStore {
    /// Rebuilds from current files and durable forgetting state on every call.
    /// The returned key identifies this projection. Callers revalidate current
    /// state before reusing it and count the actual final provider request.
    pub fn project(
        &self,
        options: MemoryProjectionOptions,
        limits: MemoryQueryLimits,
    ) -> Result<MemoryProjection, BoundaryError> {
        self.project_cancellable(options, limits, &AtomicBool::new(false))
    }
    pub fn project_cancellable(
        &self,
        options: MemoryProjectionOptions,
        limits: MemoryQueryLimits,
        cancel: &AtomicBool,
    ) -> Result<MemoryProjection, BoundaryError> {
        let query = query::prepare_query(self, &options.query, limits, true)?;
        let snapshot = self.snapshot_cancellable(cancel)?;
        let ranked = query::ranked(&snapshot, &query, true, cancel)?;
        let cache_key = digest(&encode_bounded(
            &(
                "saya-memory-projection-v1",
                self.binding(),
                &snapshot.revision,
                &snapshot.forget_revision,
                &options,
                limits,
            ),
            64 * 1024,
        )?);
        let mut projection = MemoryProjection {
            status: MemoryProjectionStatus::Empty,
            revision: snapshot.revision.clone(),
            forget_revision: snapshot.forget_revision.clone(),
            workspace: snapshot.workspace.clone(),
            routing: Vec::new(),
            selected: Vec::new(),
            routing_omitted: snapshot.entries.len(),
            facts_omitted: ranked.len(),
            truncated: !snapshot.entries.is_empty(),
            cache_key,
            text: String::new(),
        };
        if snapshot.entries.is_empty() {
            return Ok(projection);
        }
        let max_bytes = options.max_bytes.min(limits.max_projection_bytes);
        let base = Data {
            schema_version: 1,
            revision: &snapshot.revision,
            forget_revision: &snapshot.forget_revision,
            workspace: &snapshot.workspace,
            routing: &[],
            selected: &[],
            routing_omitted: usize::MAX,
            facts_omitted: usize::MAX,
        };
        let overhead = PREFIX.len() + encode_bounded(&base, 64 * 1024)?.len();
        if max_bytes <= overhead {
            projection.status = MemoryProjectionStatus::BudgetExhausted;
            return Ok(projection);
        }
        let remaining = max_bytes - overhead;
        let routing_budget = options
            .routing_bytes
            .min(limits.max_routing_bytes)
            .min(remaining);
        let mut entries = snapshot.entries.iter().collect::<Vec<_>>();
        let scores = ranked
            .iter()
            .fold(BTreeMap::<&str, u32>::new(), |mut scores, fact| {
                let score = scores.entry(&fact.entry.id).or_default();
                *score = (*score).max(fact.score);
                scores
            });
        entries.sort_by(|left, right| {
            scores
                .get(right.id.as_str())
                .copied()
                .unwrap_or(0)
                .cmp(&scores.get(left.id.as_str()).copied().unwrap_or(0))
                .then_with(|| left.id.cmp(&right.id))
        });
        for entry in entries {
            check_cancelled(cancel)?;
            if projection.routing.len() >= options.max_routing_entries.min(limits.max_results) {
                break;
            }
            let resources = entry
                .resources
                .iter()
                .take(2)
                .filter_map(|path| {
                    snapshot
                        .resources
                        .iter()
                        .find(|resource| &resource.path == path)
                        .cloned()
                })
                .collect::<Vec<_>>();
            let route = MemoryRoute {
                entry_id: entry.id.clone(),
                title: entry.title.clone(),
                scope: entry.scope.clone(),
                updated_at: entry.updated_at.clone(),
                fact_count: entry.facts.len(),
                resources_omitted: entry.resources.len().saturating_sub(resources.len()),
                resources,
            };
            let mut candidate = projection.routing.clone();
            candidate.push(route);
            if encode_bounded(&candidate, 128 * 1024)?.len() <= routing_budget {
                projection.routing = candidate;
            }
        }
        let routing_used = encode_bounded(&projection.routing, 64 * 1024)?
            .len()
            .saturating_sub(2);
        let body_budget = options
            .body_bytes
            .min(limits.max_selected_bytes)
            .min(remaining.saturating_sub(routing_used));
        for ranked in &ranked {
            check_cancelled(cancel)?;
            if projection.selected.len() >= options.max_selected_facts.min(limits.max_results) {
                break;
            }
            let start = query::excerpt_start(&ranked.fact.text, &query, cancel)?;
            let fragment = query::fragment(&snapshot, ranked.entry, ranked.fact, start)?;
            let mut candidate = projection.selected.clone();
            candidate.push(fragment.clone());
            let metadata = encode_bounded(&candidate, 128 * 1024)?.len();
            if metadata.saturating_add(20) >= body_budget {
                continue;
            }
            let end = query::prefix_end(&ranked.fact.text, start, body_budget - metadata - 20);
            if end == start {
                continue;
            }
            let mut fragment = fragment;
            fragment.text = ranked.fact.text[start..end].into();
            fragment.end_byte = end;
            fragment.complete = start == 0 && end == ranked.fact.text.len();
            candidate.pop();
            candidate.push(fragment);
            if encode_bounded(&candidate, 128 * 1024)?.len() <= body_budget {
                projection.selected = candidate;
            }
        }
        projection.routing_omitted = snapshot
            .entries
            .len()
            .saturating_sub(projection.routing.len());
        projection.facts_omitted = ranked.len().saturating_sub(projection.selected.len());
        projection.truncated = projection.routing_omitted > 0
            || projection.facts_omitted > 0
            || projection.selected.iter().any(|fact| !fact.complete)
            || projection
                .routing
                .iter()
                .any(|route| route.resources_omitted > 0);
        if projection.routing.is_empty() && projection.selected.is_empty() {
            projection.status = MemoryProjectionStatus::BudgetExhausted;
            return Ok(projection);
        }
        let data = Data {
            schema_version: 1,
            revision: &projection.revision,
            forget_revision: &projection.forget_revision,
            workspace: &projection.workspace,
            routing: &projection.routing,
            selected: &projection.selected,
            routing_omitted: projection.routing_omitted,
            facts_omitted: projection.facts_omitted,
        };
        let encoded = encode_bounded(&data, max_bytes - PREFIX.len())?;
        projection.text = format!(
            "{PREFIX}{}",
            String::from_utf8(encoded)
                .map_err(|_| error("memory_format", "Memory projection encoding is invalid"))?
        );
        projection.cache_key = digest(&encode_bounded(
            &(&projection.cache_key, &projection.text),
            512 * 1024,
        )?);
        projection.status = MemoryProjectionStatus::Included;
        check_cancelled(cancel)?;
        Ok(projection)
    }
}
