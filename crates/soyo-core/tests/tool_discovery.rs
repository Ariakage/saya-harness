mod support;

use serde_json::{Value, json};
use soyo_core::{
    approvals::Preauthorizations,
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation, Store, Tool,
        ToolContext, ToolDefinition,
    },
    config::Environment,
    contract::{
        BoundaryError, ErrorInfo, HistoryItem, ModelProtocol, PermissionMode, Run, RunId,
        RunStatus, Session, SessionId, ToolCall, ToolResult, WorkMode,
    },
    credentials::SecretRegistry,
    mcp::config::{
        McpDeferMode, McpLimitSettings, McpServerSettings, McpSettings, McpTransportSettings,
        StdioMcpSettings,
    },
    memory::MemoryStore,
    permissions::{PermissionConfig, Policy},
    registry::{ExternalAccess, mcp_model_name},
    review::ReviewConfig,
    runtime::{AgentRuntime, RuntimeLimits, RuntimeSetup},
    tools::{
        exec::ExecConfig,
        read::{ReadLimits, ReadTools},
    },
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::{Future, pending},
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
};

struct NoCancel;
impl Cancellation for NoCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}

struct Stream(Option<Vec<HistoryItem>>);
impl ModelStream for Stream {
    fn next<'a>(&'a mut self, _: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            Ok(self
                .0
                .take()
                .map(|items| ModelUpdate::Finished { items, usage: None }))
        })
    }
}

type TurnHandler =
    Box<dyn FnMut(usize, &ModelRequest) -> Result<Vec<HistoryItem>, BoundaryError> + Send>;

struct ScriptedModel {
    handler: Mutex<TurnHandler>,
    turn: Mutex<usize>,
    seen: Mutex<Vec<ModelRequest>>,
}

impl ScriptedModel {
    fn from_turns(turns: Vec<Vec<HistoryItem>>) -> Self {
        let mut queue: VecDeque<Vec<HistoryItem>> = turns.into();
        Self {
            handler: Mutex::new(Box::new(move |_, _| {
                queue.pop_front().ok_or_else(|| err("unexpected_request"))
            })),
            turn: Mutex::new(0),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn from_fn<F>(f: F) -> Self
    where
        F: FnMut(usize, &ModelRequest) -> Result<Vec<HistoryItem>, BoundaryError> + Send + 'static,
    {
        Self {
            handler: Mutex::new(Box::new(f)),
            turn: Mutex::new(0),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.seen.lock().unwrap().clone()
    }
}

impl Model for ScriptedModel {
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            self.seen.lock().unwrap().push(request.clone());
            let mut turn = self.turn.lock().unwrap();
            let idx = *turn;
            *turn += 1;
            let items = (self.handler.lock().unwrap())(idx, request)?;
            Ok(Box::new(Stream(Some(items))) as Box<dyn ModelStream>)
        })
    }
}

fn err(code: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: code.into(),
        },
        None,
        None,
    )
}

struct FakeMcpTool {
    remote_name: String,
    description: String,
    schema: Value,
    examples: Vec<Value>,
    calls: Arc<Mutex<Vec<(String, String)>>>,
}

impl Tool for FakeMcpTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.remote_name.clone(),
            description: self.description.clone(),
            input_schema: self.schema.clone(),
            examples: self.examples.clone(),
        }
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        _: &'a ToolContext,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, ToolResult> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push((call.name.clone(), call.arguments.clone()));
            Ok(ToolResult {
                ok: true,
                data: Some(json!({"executed": self.remote_name, "args": call.arguments})),
                error: None,
                truncated: false,
                media_refs: Vec::new(),
            })
        })
    }
}

fn fake_mcp_tool(
    remote_name: &str,
    description: &str,
    schema: Value,
    examples: Vec<Value>,
    calls: Arc<Mutex<Vec<(String, String)>>>,
) -> Arc<dyn Tool> {
    Arc::new(FakeMcpTool {
        remote_name: remote_name.into(),
        description: description.into(),
        schema,
        examples,
        calls,
    })
}

fn invoice_example() -> Value {
    json!({"month": "2026-11", "format": "csv"})
}

fn fixture(root: &Path, work_mode: WorkMode, permissions: PermissionMode) -> (Session, Run) {
    let session = Session {
        id: SessionId::new(),
        workspace: root.into(),
        work_mode,
        profile: "fake".into(),
        protocol: ModelProtocol::Responses,
        model: "fake".into(),
        history: vec![HistoryItem::UserText("run deferred mcp task".into())],
    };
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode,
        permissions,
    };
    (session, run)
}

fn build_runtime(
    root: &Path,
    run: Run,
    model: Arc<dyn Model>,
    permissions_cfg: PermissionConfig,
) -> Result<AgentRuntime, Box<dyn std::error::Error>> {
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    let policy = Arc::new(Policy::new(root, permissions_cfg)?);
    let files = Arc::new(ReadTools::new(root, &[], false, ReadLimits::default())?);
    Ok(AgentRuntime::new(RuntimeSetup {
        run,
        model,
        store,
        policy,
        files,
        cancellation: Arc::new(NoCancel),
        review: ReviewConfig::new("fake".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "discovery test".into(),
        confirmation: None,
        limits: RuntimeLimits::default(),
    })?)
}

fn call(id: &str, name: &str, args: Value) -> HistoryItem {
    HistoryItem::ToolCall(ToolCall {
        call_id: id.into(),
        name: name.into(),
        arguments: args.to_string(),
    })
}

fn dummy_server_settings(eager_tools: BTreeSet<String>) -> McpServerSettings {
    McpServerSettings {
        enabled: false,
        transport: McpTransportSettings::Stdio(StdioMcpSettings {
            argv: vec!["true".into()],
            cwd: None,
            env: BTreeMap::new(),
        }),
        read_only_tools: BTreeSet::new(),
        eager_tools,
        tool_examples: BTreeMap::new(),
        limits: McpLimitSettings::default(),
    }
}

fn register_fake_tools(
    runtime: &AgentRuntime,
    server: &str,
    revision: &str,
    count: usize,
    read_only_count: usize,
    calls: Arc<Mutex<Vec<(String, String)>>>,
) {
    let tools: Vec<(Arc<dyn Tool>, ExternalAccess)> = (0..count)
        .map(|i| {
            let (name, desc, schema) = if i == count - 1 {
                (
                    "invoice_export".to_owned(),
                    "Export finalized monthly invoice statements as CSV".to_owned(),
                    json!({
                        "type": "object",
                        "properties": {
                            "month": { "type": "string" },
                            "format": { "type": "string" }
                        },
                        "required": ["month"],
                        "additionalProperties": false
                    }),
                )
            } else {
                (
                    format!("item_op_{i:02}"),
                    format!("Perform synthetic catalog action number {i}"),
                    json!({
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" }
                        },
                        "additionalProperties": false
                    }),
                )
            };
            let access = if i < read_only_count {
                ExternalAccess::ReadOnly
            } else {
                ExternalAccess::Unclassified
            };
            let examples = if name == "invoice_export" {
                vec![invoice_example()]
            } else {
                Vec::new()
            };
            (
                fake_mcp_tool(&name, &desc, schema, examples, calls.clone()),
                access,
            )
        })
        .collect();
    runtime.replace_external(server, revision, tools).unwrap();
}

#[tokio::test]
async fn small_mcp_catalog_at_or_below_threshold_keeps_full_injection()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), WorkMode::Work, PermissionMode::Bypass);
    let model = Arc::new(ScriptedModel::from_turns(vec![vec![
        HistoryItem::AssistantText("done".into()),
    ]]));
    let mut runtime = build_runtime(temp.path(), run, model.clone(), PermissionConfig::default())?;
    let calls = Arc::new(Mutex::new(Vec::new()));
    register_fake_tools(&runtime, "alpha", "r1", 8, 8, calls);

    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Completed);
    let seen = model.requests();
    assert_eq!(seen.len(), 1);
    let names: Vec<_> = seen[0].tools.iter().map(|t| t.name.as_str()).collect();
    assert!(!names.contains(&"tool_search"));
    assert!(!names.contains(&"tool_describe"));
    assert!(!seen[0].instructions.contains("# On-demand tool discovery"));
    let mcp_names: Vec<_> = names.iter().filter(|n| n.starts_with("mcp__")).collect();
    assert_eq!(mcp_names.len(), 8);
    Ok(())
}

#[tokio::test]
async fn large_mcp_catalog_defers_schemas_until_search_and_describe_while_preserving_prefix()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (mut session, run) = fixture(temp.path(), WorkMode::Work, PermissionMode::Bypass);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let target_name = mcp_model_name("billing", "invoice_export")?;
    let target_for_model = target_name.clone();

    let model = Arc::new(ScriptedModel::from_fn(move |turn, req| match turn {
        0 => {
            let names: Vec<_> = req.tools.iter().map(|t| t.name.as_str()).collect();
            assert!(names.contains(&"tool_search"));
            assert!(names.contains(&"tool_describe"));
            assert!(req.instructions.contains("# On-demand tool discovery"));
            assert!(
                !names.iter().any(|n| n.starts_with("mcp__")),
                "Turn 1 must not eagerly inject any of the 24 deferred MCP schemas"
            );
            Ok(vec![call(
                "c1",
                "tool_search",
                json!({"query": "invoice export"}),
            )])
        }
        1 => {
            assert!(
                !req.tools.iter().any(|t| t.name.starts_with("mcp__")),
                "tool_search alone must not expand MCP schemas"
            );
            Ok(vec![call(
                "c2",
                "tool_describe",
                json!({"names": [target_for_model]}),
            )])
        }
        2 => {
            let names: Vec<_> = req.tools.iter().map(|t| t.name.as_str()).collect();
            assert!(
                names.contains(&target_for_model.as_str()),
                "Turn 3 must attach the described target MCP tool"
            );
            // The deferred tool's examples reached the model through tool_describe...
            let described = req
                .history
                .iter()
                .find_map(|item| match item {
                    HistoryItem::ToolResult { call_id, result } if call_id == "c2" => {
                        result.data.clone()
                    }
                    _ => None,
                })
                .expect("tool_describe result");
            assert_eq!(
                described["tools"][0]["examples"],
                json!([invoice_example()])
            );
            // ...and the expanded definition carries them for provider projection.
            assert_eq!(req.tools.last().unwrap().examples, vec![invoice_example()]);
            // Turn 1 core tools never leak the deferred tool's examples.
            assert!(req.tools[..req.tools.len() - 1].iter().all(|tool| {
                tool.name.starts_with("mcp__") || !tool.description.contains("2026-11")
            }));
            let mcp_in_req: Vec<_> = names.iter().filter(|n| n.starts_with("mcp__")).collect();
            assert_eq!(mcp_in_req.len(), 1);
            Ok(vec![call(
                "c3",
                &target_for_model,
                json!({"month": "2026-11", "format": "csv"}),
            )])
        }
        3 => Ok(vec![HistoryItem::AssistantText("invoice exported".into())]),
        _ => Err(err("unexpected_turn")),
    }));

    let mut runtime = build_runtime(temp.path(), run, model.clone(), PermissionConfig::default())?;
    register_fake_tools(&runtime, "billing", "r1", 24, 24, calls.clone());

    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Completed);
    assert_eq!(result.text.as_deref(), Some("invoice exported"));

    let requests = model.requests();
    assert_eq!(requests.len(), 4);

    // Core instructions and core tool prefix 0..K remain invariant across dynamic expansion.
    assert_eq!(requests[0].instructions, requests[1].instructions);
    assert_eq!(requests[0].instructions, requests[2].instructions);
    let prefix_len = requests[0].tools.len();
    assert_eq!(
        requests[2].tools[..prefix_len]
            .iter()
            .map(|t| &t.name)
            .collect::<Vec<_>>(),
        requests[0]
            .tools
            .iter()
            .map(|t| &t.name)
            .collect::<Vec<_>>()
    );
    assert_eq!(requests[2].tools.last().unwrap().name, target_name);

    let executed = calls.lock().unwrap();
    assert_eq!(executed.len(), 1);
    assert_eq!(executed[0].0, "invoice_export");
    assert_eq!(
        serde_json::from_str::<Value>(&executed[0].1)?,
        json!({"month": "2026-11", "format": "csv"})
    );
    Ok(())
}

#[tokio::test]
async fn mcp_defer_overrides_and_eager_tools_are_respected()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let calls = Arc::new(Mutex::new(Vec::new()));

    // 1. defer_loading = never with 24 tools -> all 24 injected directly.
    let (_session, run) = fixture(temp.path(), WorkMode::Work, PermissionMode::Bypass);
    let model = Arc::new(ScriptedModel::from_turns(vec![vec![
        HistoryItem::AssistantText("ok".into()),
    ]]));
    let mut settings = McpSettings {
        defer_loading: McpDeferMode::Never,
        defer_threshold: 8,
        servers: BTreeMap::from([("billing".into(), dummy_server_settings(BTreeSet::new()))]),
    };
    let runtime = build_runtime(temp.path(), run, model.clone(), PermissionConfig::default())?
        .with_mcp_settings(
            settings.clone(),
            Environment::default(),
            None,
            Arc::new(SecretRegistry::default()),
        )?;
    register_fake_tools(&runtime, "billing", "r1", 24, 24, calls.clone());
    let defs = runtime.model_tool_definitions(WorkMode::Work)?;
    assert!(!defs.iter().any(|d| d.name == "tool_search"));
    assert_eq!(
        defs.iter().filter(|d| d.name.starts_with("mcp__")).count(),
        24
    );
    drop(model);

    // 2. defer_loading = always with 2 tools + eager_tools = ["item_op_00"] -> only item_op_00 eager.
    let (mut session, run) = fixture(temp.path(), WorkMode::Work, PermissionMode::Bypass);
    let model = Arc::new(ScriptedModel::from_turns(vec![vec![
        HistoryItem::AssistantText("ok".into()),
    ]]));
    settings.defer_loading = McpDeferMode::Always;
    settings.servers.insert(
        "billing".into(),
        dummy_server_settings(BTreeSet::from(["item_op_00".into()])),
    );
    let mut runtime = build_runtime(temp.path(), run, model.clone(), PermissionConfig::default())?
        .with_mcp_settings(
            settings.clone(),
            Environment::default(),
            None,
            Arc::new(SecretRegistry::default()),
        )?;
    register_fake_tools(&runtime, "billing", "r1", 2, 2, calls.clone());
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Completed);
    let req = &model.requests()[0];
    let mcp_names: Vec<_> = req
        .tools
        .iter()
        .filter(|t| t.name.starts_with("mcp__"))
        .map(|t| t.name.as_str())
        .collect();
    let expected_eager = mcp_model_name("billing", "item_op_00")?;
    assert_eq!(mcp_names, vec![expected_eager.as_str()]);
    assert!(req.tools.iter().any(|t| t.name == "tool_search"));
    assert!(req.tools.iter().any(|t| t.name == "tool_describe"));

    // 3. defer_threshold = 4 with 5 tools under Auto -> deferred active.
    let (_s, run) = fixture(temp.path(), WorkMode::Work, PermissionMode::Bypass);
    let model = Arc::new(ScriptedModel::from_turns(vec![vec![
        HistoryItem::AssistantText("ok".into()),
    ]]));
    settings.defer_loading = McpDeferMode::Auto;
    settings.defer_threshold = 4;
    settings
        .servers
        .insert("billing".into(), dummy_server_settings(BTreeSet::new()));
    let runtime = build_runtime(temp.path(), run, model, PermissionConfig::default())?
        .with_mcp_settings(
            settings,
            Environment::default(),
            None,
            Arc::new(SecretRegistry::default()),
        )?;
    register_fake_tools(&runtime, "billing", "r1", 5, 5, calls);
    let defs = runtime.model_tool_definitions(WorkMode::Work)?;
    assert!(defs.iter().any(|d| d.name == "tool_search"));
    assert!(!defs.iter().any(|d| d.name.starts_with("mcp__")));
    Ok(())
}

#[tokio::test]
async fn plan_mode_hides_unclassified_tools_and_catalog_replacement_drops_removed_tools()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let unclassified_target = mcp_model_name("billing", "invoice_export")?;
    let readonly_target = mcp_model_name("billing", "item_op_00")?;
    let unclassified_for_model = unclassified_target.clone();
    let readonly_for_model = readonly_target.clone();

    // In PLAN mode with 10 read-only tools + 2 unclassified tools:
    // tool_search must omit unclassified tools and tool_describe must return plan_read_only.
    let (mut session, run) = fixture(temp.path(), WorkMode::Plan, PermissionMode::Auto);
    let model = Arc::new(ScriptedModel::from_fn(move |turn, req| match turn {
        0 => Ok(vec![call(
            "c1",
            "tool_describe",
            json!({"names": [unclassified_for_model, readonly_for_model]}),
        )]),
        1 => {
            let names: Vec<_> = req.tools.iter().map(|t| t.name.as_str()).collect();
            assert!(!names.contains(&unclassified_for_model.as_str()));
            assert!(names.contains(&readonly_for_model.as_str()));
            let tool_msg = req.history.iter().find_map(|item| match item {
                HistoryItem::ToolResult { call_id, result } if call_id == "c1" => Some(result),
                _ => None,
            });
            let data = tool_msg.unwrap().data.as_ref().unwrap();
            assert_eq!(
                data["unavailable"],
                json!([{"name": unclassified_for_model, "reason": "plan_read_only"}])
            );
            Ok(vec![HistoryItem::AssistantText("checked in plan".into())])
        }
        _ => Err(err("unexpected_turn")),
    }));
    let mut runtime = build_runtime(temp.path(), run, model.clone(), PermissionConfig::default())?;
    register_fake_tools(&runtime, "billing", "r1", 12, 10, calls.clone());
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Completed);

    // Explicit deny rule on tool_search blocks it even in Bypass mode.
    let (mut session, run) = fixture(temp.path(), WorkMode::Work, PermissionMode::Bypass);
    let model = Arc::new(ScriptedModel::from_turns(vec![
        vec![call("c1", "tool_search", json!({"query": "invoice"}))],
        vec![HistoryItem::AssistantText("blocked".into())],
    ]));
    let deny_cfg = PermissionConfig::from_toml(
        r#"
[[rules]]
effect = "deny"
tool = "tool_search"
"#,
    )?;
    let mut runtime = build_runtime(temp.path(), run, model.clone(), deny_cfg)?;
    register_fake_tools(&runtime, "billing", "r1", 12, 12, calls.clone());
    let result = runtime.run(&mut session).await?;
    assert_eq!(result.status, RunStatus::Completed);
    let req2 = &model.requests()[1];
    let denied_res = req2
        .history
        .iter()
        .find_map(|item| match item {
            HistoryItem::ToolResult { call_id, result } if call_id == "c1" => Some(result),
            _ => None,
        })
        .unwrap();
    assert!(!denied_res.ok);
    assert_eq!(
        denied_res.error.as_ref().map(|e| e.code.as_str()),
        Some("permission_denied")
    );

    // Replace external catalog after expansion: removed tool disappears from model_tool_definitions.
    let (mut s2, _) = fixture(temp.path(), WorkMode::Work, PermissionMode::Bypass);
    let mut runtime2 = build_runtime(
        temp.path(),
        Run {
            id: RunId::new(),
            session_id: s2.id,
            work_mode: WorkMode::Work,
            permissions: PermissionMode::Bypass,
        },
        Arc::new(ScriptedModel::from_turns(vec![
            vec![call(
                "d1",
                "tool_describe",
                json!({"names": [unclassified_target]}),
            )],
            vec![HistoryItem::AssistantText("expanded".into())],
        ])),
        PermissionConfig::default(),
    )?;
    register_fake_tools(&runtime2, "billing", "r1", 12, 12, calls.clone());
    let res2 = runtime2.run(&mut s2).await?;
    assert_eq!(res2.status, RunStatus::Completed);
    assert!(
        runtime2
            .model_tool_definitions(WorkMode::Work)?
            .iter()
            .any(|d| d.name == unclassified_target)
    );
    // Now replace catalog without invoice_export (only 10 item_op tools)
    let replacement: Vec<(Arc<dyn Tool>, ExternalAccess)> = (0..10)
        .map(|i| {
            (
                fake_mcp_tool(
                    &format!("item_op_{i:02}"),
                    &format!("Perform synthetic catalog action number {i}"),
                    json!({
                        "type": "object",
                        "properties": { "id": { "type": "string" } },
                        "additionalProperties": false
                    }),
                    Vec::new(),
                    calls.clone(),
                ),
                ExternalAccess::ReadOnly,
            )
        })
        .collect();
    runtime2.replace_external("billing", "r2", replacement)?;
    let defs_after = runtime2.model_tool_definitions(WorkMode::Work)?;
    assert!(!defs_after.iter().any(|d| d.name == unclassified_target));
    // Re-adding invoice_export in r3 keeps it deferred until described again.
    register_fake_tools(&runtime2, "billing", "r3", 12, 12, calls);
    let defs_r3 = runtime2.model_tool_definitions(WorkMode::Work)?;
    assert!(!defs_r3.iter().any(|d| d.name == unclassified_target));
    Ok(())
}

#[tokio::test]
async fn manual_approval_gates_tool_search_and_parallel_search_batch_succeeds()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let calls = Arc::new(Mutex::new(Vec::new()));

    // Manual mode without interactive confirmation returns NeedsConfirmation.
    let (mut session, run) = fixture(temp.path(), WorkMode::Work, PermissionMode::Manual);
    let model = Arc::new(ScriptedModel::from_turns(vec![vec![call(
        "c1",
        "tool_search",
        json!({"query": "invoice"}),
    )]]));
    let mut runtime = build_runtime(temp.path(), run, model, PermissionConfig::default())?;
    register_fake_tools(&runtime, "billing", "r1", 12, 12, calls.clone());
    let res = runtime.run(&mut session).await?;
    assert_eq!(res.status, RunStatus::NeedsConfirmation);

    // Auto mode parallel batch of two tool_search calls succeeds.
    let (mut session, run) = fixture(temp.path(), WorkMode::Work, PermissionMode::Auto);
    let model = Arc::new(ScriptedModel::from_turns(vec![
        vec![
            call("p1", "tool_search", json!({"query": "invoice"})),
            call("p2", "tool_search", json!({"query": "item_op_00"})),
        ],
        vec![HistoryItem::AssistantText("parallel ok".into())],
    ]));
    let mut runtime = build_runtime(temp.path(), run, model.clone(), PermissionConfig::default())?;
    register_fake_tools(&runtime, "billing", "r1", 12, 12, calls);
    let res = runtime.run(&mut session).await?;
    assert_eq!(res.status, RunStatus::Completed);
    let req2 = &model.requests()[1];
    let p1_ok = req2.history.iter().any(|item| {
        matches!(item, HistoryItem::ToolResult { call_id, result } if call_id == "p1" && result.ok)
    });
    let p2_ok = req2.history.iter().any(|item| {
        matches!(item, HistoryItem::ToolResult { call_id, result } if call_id == "p2" && result.ok)
    });
    assert!(p1_ok && p2_ok);
    Ok(())
}
