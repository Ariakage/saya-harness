//! Offline inspection only; does not call a model or execute a tool.
use saya_core::{
    boundary::ToolDefinition,
    contract::{PermissionMode, WorkMode},
    prompt,
};
use sha2::{Digest, Sha256};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().ok_or(
        "usage: prompt_preview plan|build|code|work auto|manual|bypass [tool_names...] OR --review",
    )?;
    let output = if mode == "--review" {
        if args.next().is_some() {
            return Err("--review accepts no other arguments".into());
        }
        {
            let instructions = prompt::review_instructions();
            let fingerprint: String = Sha256::digest(instructions.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            serde_json::json!({"prompt_version":prompt::VERSION,"purpose":"review","fingerprint":fingerprint,"sections":["review"],"instructions":instructions})
        }
    } else {
        let mode = match mode.as_str() {
            "plan" => WorkMode::Plan,
            "build" => WorkMode::Build,
            "code" => WorkMode::Code,
            "work" => WorkMode::Work,
            _ => return Err("unknown work mode".into()),
        };
        let permissions = match args.next().as_deref() {
            Some("auto") => PermissionMode::Auto,
            Some("manual") => PermissionMode::Manual,
            Some("bypass") => PermissionMode::Bypass,
            _ => return Err("expected auto|manual|bypass".into()),
        };
        // Names are inspection inputs; these placeholder schemas do not expose tools.
        let tools: Vec<_> = args
            .map(|name| ToolDefinition {
                name,
                description: String::new(),
                input_schema: serde_json::json!({"type":"object"}),
            })
            .collect();
        let bundle = prompt::execution(mode, permissions, &tools)?;
        serde_json::json!({"prompt_version":bundle.version,"purpose":"execution","fingerprint":bundle.fingerprint,"sections":bundle.section_ids,"instructions":bundle.instructions})
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
