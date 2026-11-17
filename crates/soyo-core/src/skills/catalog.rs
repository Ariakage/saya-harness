use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillCatalogCursor {
    pub view_sha256: String,
    pub offset: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillCatalogItem {
    pub name: String,
    pub description: String,
    pub source_sha256: String,
    pub binding_sha256: String,
    pub origin: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillCatalogPage {
    pub view_sha256: String,
    pub total: usize,
    pub offset: usize,
    pub entries: Vec<SkillCatalogItem>,
    pub omitted: usize,
    pub next_cursor: Option<SkillCatalogCursor>,
}
impl SkillManager {
    /// The page contains safe metadata only. It never loads an inactive body.
    /// Cursors bind the current redacted metadata view and are invalidated by
    /// catalog changes or credential-driven metadata redaction changes.
    pub fn catalog_page(
        &self,
        cursor: Option<&SkillCatalogCursor>,
        max_items: usize,
        max_bytes: usize,
    ) -> Result<SkillCatalogPage, BoundaryError> {
        if max_items == 0 || max_items > 64 || !(1024..=16 * 1024).contains(&max_bytes) {
            return Err(error(
                "invalid_args",
                "Skill catalog page limits are invalid",
            ));
        }
        let entries = self
            .catalog()?
            .into_iter()
            .map(|metadata| {
                let mut description = metadata.description;
                while description.len() > 512 {
                    description.pop();
                }
                let origin = match metadata.origin {
                    SkillOrigin::Project { scope, .. } => scope,
                    SkillOrigin::Plugin {
                        plugin_id,
                        package_digest,
                        ..
                    } => format!("plugin:{plugin_id}@{package_digest}"),
                };
                SkillCatalogItem {
                    name: metadata.name,
                    description,
                    source_sha256: metadata.source_sha256,
                    binding_sha256: metadata.binding_sha256,
                    origin,
                }
            })
            .collect::<Vec<_>>();
        let encoded = serde_json::to_vec(&entries)
            .map_err(|_| error("skill_catalog", "Skill catalog view cannot be encoded"))?;
        let view_sha256 = hash(&encoded);
        let offset = cursor.map_or(0, |cursor| cursor.offset);
        if cursor.is_some_and(|cursor| cursor.view_sha256 != view_sha256) || offset > entries.len()
        {
            return Err(error(
                "cursor_invalid",
                "Skill catalog cursor belongs to a changed view",
            ));
        }
        let total = entries.len();
        let mut page = SkillCatalogPage {
            view_sha256: view_sha256.clone(),
            total,
            offset,
            entries: Vec::new(),
            omitted: total - offset,
            next_cursor: None,
        };
        for entry in entries.into_iter().skip(offset).take(max_items) {
            page.entries.push(entry);
            let end = offset + page.entries.len();
            page.omitted = total - end;
            page.next_cursor = (end < total).then_some(SkillCatalogCursor {
                view_sha256: view_sha256.clone(),
                offset: end,
            });
            let bytes = serde_json::to_vec(&page)
                .map_err(|_| error("skill_catalog", "Skill catalog page cannot be encoded"))?
                .len();
            if bytes > max_bytes {
                page.entries.pop();
                break;
            }
        }
        let end = offset + page.entries.len();
        page.omitted = total - end;
        page.next_cursor = (end < total).then_some(SkillCatalogCursor {
            view_sha256,
            offset: end,
        });
        if page.entries.is_empty() && page.omitted > 0 {
            return Err(error(
                "skill_page_limit",
                "Skill catalog page cannot fit the next metadata entry",
            ));
        }
        Ok(page)
    }
}
