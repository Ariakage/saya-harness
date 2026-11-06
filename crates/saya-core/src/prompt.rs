//! Versioned Saya prompts. Only runtime-selected enums and validated registry
//! names enter execution instructions; task/file/tool content stays in history.
//! These instructions guide models. Policy, budgets and dispatch enforce rules.
use crate::{
    boundary::ToolDefinition,
    contract::{BoundaryError, ErrorInfo, HistoryItem, PermissionMode, Run, Session, WorkMode},
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const VERSION: &str = "saya-prompts-v1";
const CORE: &str = include_str!("../prompts/v1/core.md");
const TOOLS: &str = include_str!("../prompts/v1/tools.md");
const BACKGROUND: &str = include_str!("../prompts/v1/background.md");
const REVIEW: &str = include_str!("../prompts/v1/review.md");

/// Inspectable assembly; the fingerprint identifies text, not model behavior.
#[derive(Debug)]
pub struct PromptBundle {
    pub version: &'static str,
    pub section_ids: Vec<&'static str>,
    pub instructions: String,
    pub fingerprint: String,
}
/// Mode-specific duties, independently reusable for prompt inspection.
pub fn work_mode_instructions(mode: WorkMode) -> &'static str {
    match mode {
        WorkMode::Plan => include_str!("../prompts/v1/modes/plan.md"),
        WorkMode::Build => include_str!("../prompts/v1/modes/build.md"),
        WorkMode::Code => include_str!("../prompts/v1/modes/code.md"),
        WorkMode::Work => include_str!("../prompts/v1/modes/work.md"),
    }
}
fn permission_instructions(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Auto => include_str!("../prompts/v1/permissions/auto.md"),
        PermissionMode::Manual => include_str!("../prompts/v1/permissions/manual.md"),
        PermissionMode::Bypass => include_str!("../prompts/v1/permissions/bypass.md"),
    }
}
/// Assemble execution instructions with stable core first. Names must be unique,
/// nonempty ASCII identifiers of at most 64 bytes (letters/digits/_/-; first
/// character a letter). Invalid registries return an error, never partial text.
/// Tool descriptions/schemas remain in ModelRequest.tools and are not promoted
/// to instruction text. This pure function performs no I/O or authorization.
pub fn execution(
    mode: WorkMode,
    permissions: PermissionMode,
    tools: &[ToolDefinition],
) -> Result<PromptBundle, BoundaryError> {
    let mut names = BTreeSet::new();
    for tool in tools {
        let name = tool.name.as_str();
        if name.len() > 64
            || !name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || !names.insert(name)
        {
            return Err(BoundaryError(
                ErrorInfo {
                    code: "invalid_tool_registry".into(),
                    message: "tool names must be unique ASCII identifiers".into(),
                },
                None,
                None,
            ));
        }
    }
    let mut sections = vec![
        ("core", CORE),
        ("tools", TOOLS),
        ("work_mode", work_mode_instructions(mode)),
        ("permissions", permission_instructions(permissions)),
    ];
    if names.iter().any(|name| {
        matches!(
            *name,
            "background_start" | "background_status" | "background_cancel"
        )
    }) {
        sections.push(("background", BACKGROUND));
    }
    let mut instructions = format!("Saya prompt pack: {VERSION}\n\n");
    let mut section_ids = Vec::new();
    for (id, text) in sections {
        section_ids.push(id);
        instructions.push_str(text.trim());
        instructions.push_str("\n\n");
    }
    section_ids.push("available_tools");
    instructions.push_str("# Available tool names (the attached schemas are authoritative)\n");
    let names: Vec<_> = names.into_iter().collect();
    instructions.push_str(&serde_json::to_string(&names).map_err(|_| {
        BoundaryError(
            ErrorInfo {
                code: "prompt_error".into(),
                message: "cannot encode tool registry".into(),
            },
            None,
            None,
        )
    })?);
    instructions.push('\n');
    let fingerprint = Sha256::digest(instructions.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(PromptBundle {
        version: VERSION,
        section_ids,
        instructions,
        fingerprint,
    })
}
/// A separate, tool-free review policy. Never append execution mode prompts,
/// project instructions, or task transcripts to this instruction string.
pub fn review_instructions() -> &'static str {
    REVIEW
}

/// Encode known per-request facts as data, never as high-priority instructions.
/// The caller supplies the effective run mode after applying pending changes.
/// No filesystem probes, configuration loading, credentials or history copying.
pub fn runtime_context(session: &Session, run: &Run) -> Result<HistoryItem, BoundaryError> {
    if session.id != run.session_id {
        return Err(BoundaryError(
            ErrorInfo {
                code: "invalid_run".into(),
                message: "prompt context session mismatch".into(),
            },
            None,
            None,
        ));
    }
    let workspace = session.workspace.to_str().ok_or_else(|| {
        BoundaryError(
            ErrorInfo {
                code: "invalid_prompt_context".into(),
                message: "workspace is not valid UTF-8".into(),
            },
            None,
            None,
        )
    })?;
    let data = serde_json::json!({
        "saya_runtime_context_v1": {
            "workspace": workspace,
            "profile": session.profile,
            "model": session.model,
            "protocol": session.protocol,
            "work_mode": run.work_mode,
            "permission_mode": run.permissions
        }
    });
    Ok(HistoryItem::UserText(data.to_string()))
}
