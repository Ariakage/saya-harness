import type { Channel } from "@tauri-apps/api/core";
import { mockIPC } from "@tauri-apps/api/mocks";
import type {
  ActiveSessionDto_Serialize,
  ApprovalRequestDto_Deserialize,
  BootstrapStateDto_Serialize,
  CredentialRecordDto_Serialize,
  DesktopStreamEnvelope_Deserialize,
  ExportFormatDto,
  ExportScopeDto,
  InputRequestDto,
  McpServerDto_Serialize,
  ModuleStatusDto_Serialize,
  PermissionModeDto,
  SandboxBackendDto,
  SessionPageDto_Serialize,
  StagedAttachmentDto,
  WorkModeDto,
} from "../bindings";

export type PreviewScene =
  | "home"
  | "conversation"
  | "markdown"
  | "approval"
  | "clarification"
  | "peripheral"
  | "usage"
  | "effort"
  | "browser"
  | "subagent"
  | "settings";

let installed = false;
let currentScene: PreviewScene = "conversation";
let activeAbort: { cancelled: boolean } | null = null;
let sequenceCounter = 1;

const BASE_TIMESTAMP = Math.floor(Date.now() / 1000);

const CONVERSATION_SESSION: ActiveSessionDto_Serialize = {
  id: "01930000-0000-7000-8000-000000000001",
  title: "Implement desktop UI preview with Vite HMR",
  workspace: "/workspace/soyo-harness",
  work_mode: "code",
  profile: "anthropic-official",
  model: "claude-sonnet-4-5",
  history_omitted: false,
  last_usage: {
    input_tokens: 1420,
    output_tokens: 380,
    cached_input_tokens: 512,
    estimated: false,
  },
  todos: [
    {
      id: "todo-1",
      content: "Wire Channel streaming in soyo-desktop",
      status: "completed",
    },
    {
      id: "todo-2",
      content: "Verify Streamdown + Mermaid budget fallback",
      status: "in_progress",
    },
    {
      id: "todo-3",
      content: "Add approval dialog round-trip (Stage 4)",
      status: "pending",
    },
  ],
  messages: [
    {
      id: "m-user-1",
      role: "user",
      label: "You",
      text: "Check the runtime boundary for soyo-desktop.",
      streaming: false,
      truncated: false,
      tool_call_id: null,
      tool_name: null,
      tool_summary: null,
      tool_ok: null,
      media_refs: [],
    },
    {
      id: "m-tool-1",
      role: "tool",
      label: "Tool · read_file",
      text: "crates/soyo-app/src/lib.rs (lines 1-90): spawn_build_runtime + Launch",
      streaming: false,
      truncated: false,
      tool_call_id: "call_read_1",
      tool_name: "read_file",
      tool_summary: "crates/soyo-app/src/lib.rs · 90 lines",
      tool_ok: true,
      media_refs: [],
    },
    {
      id: "m-ast-1",
      role: "assistant",
      label: "Assistant",
      text: `In-process linking directly invokes \`soyo_app::spawn_build_runtime\`[^runtime] over \`Channel<DesktopStreamEnvelope>\`:

- Blocking SQLite reads stay isolated in \`offload_boundary\`.
- No remote assets or keyring plugins are loaded.

\`\`\`rust
pub async fn cancel_run(&self) -> bool {
    self.runtime.cancel_run()
}
\`\`\`

Type any prompt below to watch live **Channel** streaming and cancellation in your browser!

[^runtime]: Implemented in \`desktop/src-tauri/src/runtime_bridge.rs\`.`,
      streaming: false,
      truncated: false,
      tool_call_id: null,
      tool_name: null,
      tool_summary: null,
      tool_ok: null,
      media_refs: [],
    },
    {
      id: "m-tool-2",
      role: "tool",
      label: "Tool · edit_file",
      text: `--- a/desktop/src/App.tsx
+++ b/desktop/src/App.tsx
@@ -855,7 +855,10 @@ export function App()
 const [copiedId, setCopiedId] = useState<string | null>(null);
+const [editDraft, setEditDraft] = useState<Draft | null>(null);
 const [forceHome, setForceHome] = useState(false);
@@ -912,4 +915,6 @@ function MessageRow()
-<Pencil className="h-3 w-3 opacity-50" />
+<button onClick={() => setEditDraft(msg.text)} title="Edit">
+  <Pencil className="h-3 w-3" />
+</button>`,
      streaming: false,
      truncated: false,
      tool_call_id: "call_edit_1",
      tool_name: "edit_file",
      tool_summary: "desktop/src/App.tsx",
      tool_ok: true,
      media_refs: [],
    },
  ],
};

const MARKDOWN_SHOWCASE_SESSION: ActiveSessionDto_Serialize = {
  ...CONVERSATION_SESSION,
  id: "01930000-0000-7000-8000-000000000009",
  title: "Markdown, Math & Mermaid Budget Showcase",
  work_mode: "plan",
  messages: [
    {
      id: "md-user-1",
      role: "user",
      label: "You",
      text: "Render syntax highlighting, GFM tables, math, Mermaid diagrams, and fallback cards.",
      streaming: false,
      truncated: false,
      tool_call_id: null,
      tool_name: null,
      tool_summary: null,
      tool_ok: null,
      media_refs: [],
    },
    {
      id: "md-ast-1",
      role: "assistant",
      label: "Assistant",
      text: `### 1. Syntax Highlighting & Tables

| Capability | Engine | Budget |
| --- | --- | --- |
| Code Highlight | Shiki \`github-dark\` | 32 KiB / block |
| Math | KaTeX inline & block | 16 KiB / block |
| Diagrams | Mermaid strict | 8 KiB, <= 128 lines |

### 2. Math & HTML Safety

Inline energy $E = mc^2$ and literal HTML preservation: \`<script>alert('xss')</script>\` stays inert.

\\[
\\frac{\\partial L}{\\partial \\theta} = \\sum_{i=1}^{n} w_i x_i
\\]

### 3. Mermaid Diagram & Budget Fallback[^budget]

\`\`\`mermaid
flowchart LR
  User[Prompt] --> Bridge[RuntimeBridge]
  Bridge --> Stream[Channel Stream]
  Stream --> UI[Streamdown]
\`\`\`

\`\`\`mermaid
mindmap
  root((Disallowed type))
    fallback
\`\`\`

[^budget]: Disallowed diagram types (like \`mindmap\`) degrade safely into a **source fallback** card.`,
      streaming: false,
      truncated: false,
      tool_call_id: null,
      tool_name: null,
      tool_summary: null,
      tool_ok: null,
      media_refs: [],
    },
  ],
};

const HOME_EMPTY_SESSION: ActiveSessionDto_Serialize = {
  ...CONVERSATION_SESSION,
  id: "01930000-0000-7000-8000-000000000000",
  title: "New session",
  work_mode: "work",
  messages: [],
  todos: [],
  last_usage: null,
};

const SUBAGENT_SHOWCASE_SESSION: ActiveSessionDto_Serialize = {
  ...CONVERSATION_SESSION,
  id: "01930000-0000-7000-8000-000000000010",
  title: "Find session restore & wire preview mode",
  work_mode: "code",
  messages: [
    {
      id: "sub-user-1",
      role: "user",
      label: "You",
      text: "Find where session restore is implemented, then wire preview mode.",
      streaming: false,
      truncated: false,
      tool_call_id: null,
      tool_name: null,
      tool_summary: null,
      tool_ok: null,
      media_refs: [],
    },
    {
      id: "sub-tool-1",
      role: "tool",
      label: "subagent_explore",
      text: "grep restore_session -> done\nread_file session_store.rs -> done\nread_file runtime_bridge.rs -> running",
      streaming: false,
      truncated: false,
      tool_call_id: "call_sub_1",
      tool_name: "subagent_explore",
      tool_summary: "3 tool calls · 4.2k tok",
      tool_ok: true,
      media_refs: [],
    },
    {
      id: "sub-ast-1",
      role: "assistant",
      label: "Assistant",
      text: "Dispatched read-only `subagent_explore` to locate the session restore path while wiring preview scenes across `desktop/src/preview/browserPreview.ts`.",
      streaming: false,
      truncated: false,
      tool_call_id: null,
      tool_name: null,
      tool_summary: null,
      tool_ok: null,
      media_refs: [],
    },
  ],
};

export function isBrowserPreview(): boolean {
  return (
    typeof window !== "undefined" &&
    import.meta.env.DEV &&
    (!("__TAURI_INTERNALS__" in window) ||
      new URLSearchParams(window.location.search).get("preview") === "1")
  );
}

export const PREVIEW_APPROVAL_REQUEST: ApprovalRequestDto_Deserialize = {
  approval_id: "appr_preview_01",
  call_id: "call_exec_preview_01",
  run_id: "preview-run",
  tool: "exec",
  operation_summary: "Run workspace verification tests with cargo",
  arguments: { command: "cargo test -p soyo-core --locked" },
  formatted_call: "exec $ cargo test -p soyo-core --locked",
  action: "shell.exec",
  cwd: "/workspace/soyo-harness",
  work_mode: "code",
};

export const PREVIEW_INPUT_REQUEST: InputRequestDto = {
  input_id: "input_preview_01",
  question: "Which target bundle format should we inspect first?",
  options: ["macOS (.dmg / .app.tar.gz)", "Windows (NSIS setup)", "Linux (AppImage)"],
};

export function getPreviewScene(): PreviewScene {
  if (typeof window !== "undefined") {
    const param = new URLSearchParams(window.location.search).get("scene");
    if (
      param === "home" ||
      param === "markdown" ||
      param === "conversation" ||
      param === "approval" ||
      param === "clarification" ||
      param === "peripheral" ||
      param === "usage" ||
      param === "effort" ||
      param === "browser" ||
      param === "subagent" ||
      param === "settings"
    ) {
      currentScene = param;
    }
  }
  return currentScene;
}

export function getPreviewTab(): string | null {
  if (typeof window === "undefined") return null;
  return new URLSearchParams(window.location.search).get("tab");
}

export function setPreviewScene(scene: PreviewScene): void {
  currentScene = scene;
  if (typeof window !== "undefined") {
    const url = new URL(window.location.href);
    url.searchParams.set("scene", scene);
    window.history.replaceState({}, "", url.toString());
  }
}

function buildBootstrap(scene: PreviewScene): BootstrapStateDto_Serialize {
  const session =
    scene === "home"
      ? HOME_EMPTY_SESSION
      : scene === "subagent"
        ? SUBAGENT_SHOWCASE_SESSION
        : scene === "markdown"
          ? MARKDOWN_SHOWCASE_SESSION
          : CONVERSATION_SESSION;
  return {
    version: "0.3.0",
    config_ready: true,
    config_path: "~/.config/soyo/config.toml",
    config_error: null,
    workspace: "/workspace/soyo-harness",
    selected_profile: "anthropic-official",
    selected_model: "claude-sonnet-4-5",
    context_window_tokens: 200000,
    thinking_effort: "medium",
    work_mode: session.work_mode,
    pending_mode: null,
    permission_mode: "auto",
    sandbox_backend: "seatbelt",
    auto_compact: true,
    auto_compact_trigger_tokens: 160000,
    auto_compact_trigger_percent: 80,
    profiles: [
      {
        name: "anthropic-official",
        model: "claude-sonnet-4-5",
        protocol: "anthropic",
        context_window_tokens: 200000,
        thinking_effort: "medium",
      },
      {
        name: "openai-responses",
        model: "gpt-5.2",
        protocol: "openai_responses",
        context_window_tokens: 128000,
        thinking_effort: "high",
      },
    ],
    session,
    run_active: activeAbort !== null && !activeAbort.cancelled,
    active_run_id: activeAbort ? "preview-run" : null,
  };
}

function simulateRunStream(
  prompt: string,
  channel: Channel<DesktopStreamEnvelope_Deserialize>,
  abortToken: { cancelled: boolean }
) {
  const sessionId =
    currentScene === "markdown"
      ? MARKDOWN_SHOWCASE_SESSION.id
      : CONVERSATION_SESSION.id;
  const runId = `run_preview_${Date.now()}`;
  const reqId = `req_${Date.now()}`;
  const callId = `call_exec_${Date.now()}`;

  const emit = (payload: DesktopStreamEnvelope_Deserialize) => {
    channel.onmessage(payload);
  };

  const makeSchema = (
    payload: Extract<
      DesktopStreamEnvelope_Deserialize,
      { kind: "schema_event" }
    >["data"]["payload"]
  ): DesktopStreamEnvelope_Deserialize => ({
    kind: "schema_event",
    data: {
      schema_version: 1,
      session_id: sessionId,
      run_id: runId,
      seq: sequenceCounter++,
      timestamp: new Date().toISOString(),
      payload,
    },
  });

  const steps: Array<{ delay: number; run: () => void }> = [
    {
      delay: 20,
      run: () =>
        emit(
          makeSchema({
            type: "run.started",
            data: {
              work_mode: "code",
              permissions: "auto",
              profile: "anthropic-official",
            },
          })
        ),
    },
    {
      delay: 60,
      run: () =>
        emit({
          kind: "context",
          data: {
            entries: [
              { label: "System Prompt", tokens: 1850 },
              { label: "Project Rules", tokens: 920 },
              { label: "Skills", tokens: 640 },
              { label: "Messages", tokens: 2410 },
              { label: "Tool Outputs", tokens: 530 },
            ],
            skills_count: 4,
            estimated: true,
          },
        }),
    },
    {
      delay: 90,
      run: () =>
        emit({
          kind: "thinking",
          data: [
            {
              request_id: reqId,
              text: `Analyzing request: "${prompt.slice(0, 80)}"\nInspecting workspace context...`,
              active: true,
              truncated: false,
              available: true,
            },
          ],
        }),
    },
    {
      delay: 140,
      run: () =>
        emit(
          makeSchema({
            type: "tool.requested",
            data: {
              call_id: callId,
              name: "exec",
              args: { cmd: "git status -s" },
              summary: "git status -s",
              formatted: "$ git status -s",
            },
          })
        ),
    },
    {
      delay: 200,
      run: () =>
        emit(
          makeSchema({
            type: "tool.finished",
            data: {
              call_id: callId,
              name: "exec",
              result: {
                ok: true,
                data: { exit_code: 0 },
                error: null,
                truncated: false,
                summary: "exit 0 · 14ms",
                formatted: "M desktop/src/App.tsx",
                media_refs: [],
              },
            },
          })
        ),
    },
    {
      delay: 250,
      run: () =>
        emit({
          kind: "thinking",
          data: [
            {
              request_id: reqId,
              text: `Analyzing request: "${prompt.slice(0, 80)}"\nPreparing structured response.`,
              active: false,
              truncated: false,
              available: true,
            },
          ],
        }),
    },
    {
      delay: 290,
      run: () =>
        emit(
          makeSchema({
            type: "model.text_delta",
            data: {
              request_id: reqId,
              text: `Processed **"${prompt.slice(0, 48)}"** in preview mode.\n\n`,
            },
          })
        ),
    },
    {
      delay: 360,
      run: () =>
        emit(
          makeSchema({
            type: "model.text_delta",
            data: {
              request_id: reqId,
              text: `\`\`\`ts\nconst channelReady = true;\n\`\`\`\n`,
            },
          })
        ),
    },
    {
      delay: 420,
      run: () => {
        emit(
          makeSchema({
            type: "model.finished",
            data: {
              request_id: reqId,
              purpose: "execution",
              ok: true,
              usage: {
                input_tokens: 1580,
                output_tokens: 46,
                cached_input_tokens: 512,
                estimated: true,
              },
              error: null,
            },
          })
        );
        activeAbort = null;
        emit({
          kind: "run_completed",
          data: {
            status: "completed",
            summary_line: "claude-sonnet-4-5 · 0.42s · ↑1.6k ↓46",
            todos: CONVERSATION_SESSION.todos,
            error: null,
          },
        });
      },
    },
  ];

  for (const step of steps) {
    setTimeout(() => {
      if (abortToken.cancelled) {
        return;
      }
      step.run();
    }, step.delay);
  }
}

export function installBrowserPreviewMock(): boolean {
  if (!isBrowserPreview() || installed) {
    return installed;
  }
  installed = true;
  currentScene = getPreviewScene();

  const samplePage: SessionPageDto_Serialize = {
    page: 1,
    page_size: 20,
    items: [
      {
        id: CONVERSATION_SESSION.id,
        workspace: CONVERSATION_SESSION.workspace,
        work_mode: CONVERSATION_SESSION.work_mode,
        profile: CONVERSATION_SESSION.profile,
        title: CONVERSATION_SESSION.title,
        updated_at: BASE_TIMESTAMP - 180,
        last_status: "completed",
      },
      {
        id: MARKDOWN_SHOWCASE_SESSION.id,
        workspace: MARKDOWN_SHOWCASE_SESSION.workspace,
        work_mode: MARKDOWN_SHOWCASE_SESSION.work_mode,
        profile: "openai-responses",
        title: MARKDOWN_SHOWCASE_SESSION.title,
        updated_at: BASE_TIMESTAMP - 3600,
        last_status: "completed",
      },
      {
        id: SUBAGENT_SHOWCASE_SESSION.id,
        workspace: "/workspace/AkaMemo",
        work_mode: "code",
        profile: "anthropic-official",
        title: "项目 AGENTS.md 与文档规范 (fork)",
        updated_at: BASE_TIMESTAMP - 7200,
        last_status: "completed",
      },
      {
        id: "01930000-0000-7000-8000-000000000011",
        workspace: "/workspace/ai-gateway",
        work_mode: "work",
        profile: "anthropic-official",
        title: "导出会话到 HANDOFF 文档",
        updated_at: BASE_TIMESTAMP - 18000,
        last_status: "completed",
      },
    ],
  };

  let lastChannel: Channel<DesktopStreamEnvelope_Deserialize> | null = null;
  let previewWorkspace = CONVERSATION_SESSION.workspace;
  let previewProfile = "anthropic-official";
  let previewEffort: string | null = "medium";
  let previewBackend: SandboxBackendDto = "seatbelt";
  let previewAutoCompact = true;
  let previewTriggerTokens: number | null = 160000;

  let previewModules: ModuleStatusDto_Serialize[] = [
    {
      module: "mcp",
      description: "Managed MCP tool bridges with credential injection",
      enabled: true,
      configured_enabled: true,
      override_enabled: null,
      source: "config",
      active: true,
      issue: null,
    },
    {
      module: "memory",
      description: "Global memory store with two-phase mutation review",
      enabled: true,
      configured_enabled: true,
      override_enabled: null,
      source: "config",
      active: true,
      issue: null,
    },
    {
      module: "browser",
      description: "Isolated headless browser automation module",
      enabled: false,
      configured_enabled: false,
      override_enabled: false,
      source: "session_override",
      active: false,
      issue: null,
    },
  ];

  let previewMcpServers: McpServerDto_Serialize[] = [
    {
      name: "filesystem",
      transport: "stdio",
      enabled: true,
      target: "soyo-fs-mcp --readonly",
      scopes: ["read:workspace"],
      credential_ref: null,
    },
    {
      name: "remote-docs",
      transport: "http",
      enabled: true,
      target: "https://docs.example.internal/mcp",
      scopes: ["docs:read"],
      credential_ref: "cred_docs_01",
    },
  ];

  let previewCredentials: CredentialRecordDto_Serialize[] = [
    {
      reference: "cred_docs_01",
      server: "remote-docs",
      audience: "https://docs.example.internal",
      scopes: ["docs:read"],
      kind: "bearer_env",
      revision: 1,
      revoked: false,
      oauth_resource: null,
    },
  ];

  const initialThemeParam =
    typeof window !== "undefined"
      ? new URLSearchParams(window.location.search).get("theme")
      : null;
  let previewPrefs = {
    nickname: "ariakage" as string | null,
    avatar_data: null as string | null,
    theme: (initialThemeParam === "light" || initialThemeParam === "dark"
      ? initialThemeParam
      : "system") as string | null,
    font_size: 14 as number | null,
    conversation_font: "default" as string | null,
    reduce_motion: false as boolean | null,
    inspector_open: true as boolean | null,
  };
  let previewGlobalPrompt = "# Workspace Instructions\nFollow AGENTS.md and keep design tokens centralized.";

  const previewMemoryEntries = [
    {
      id: "mem_001",
      topic: "desktop-ui",
      text: "Lucide icons (lucide-react) are required for all desktop UI controls.",
      revision: 1,
    },
  ];
  const pendingPreviewActions = new Map<string, string>();

  const currentBootstrapState = (): BootstrapStateDto_Serialize => {
    const base = buildBootstrap(currentScene);
    const chosenProfile =
      base.profiles.find((p) => p.name === previewProfile) ?? base.profiles[0];
    return {
      ...base,
      workspace: previewWorkspace,
      selected_profile: previewProfile,
      selected_model: chosenProfile?.model ?? base.selected_model,
      context_window_tokens:
        chosenProfile?.context_window_tokens ?? base.context_window_tokens,
      thinking_effort: previewEffort,
      sandbox_backend: previewBackend,
      auto_compact: previewAutoCompact,
      auto_compact_trigger_tokens: previewTriggerTokens,
    };
  };

  mockIPC((cmd, args) => {
    switch (cmd) {
      case "list_sessions": {
        const page = (args as { page?: number } | undefined)?.page ?? 1;
        return {
          ...samplePage,
          page: page ?? 1,
        };
      }
      case "get_bootstrap_state":
        return currentBootstrapState();
      case "new_session":
        return {
          id: `01930000-0000-7000-8000-${String(Date.now()).slice(-12)}`,
          title: "New preview session",
          workspace: "/workspace/soyo-harness",
          work_mode: "work",
          profile: "anthropic-official",
          model: "claude-sonnet-4-5",
          messages: [],
          todos: [],
          history_omitted: false,
          last_usage: null,
        };
      case "submit_prompt": {
        const payload = args as {
          prompt: string;
          sessionId?: string | null;
          onEvent: Channel<DesktopStreamEnvelope_Deserialize>;
        };
        const activeSid = payload.sessionId ?? CONVERSATION_SESSION.id;
        if (activeAbort && !activeAbort.cancelled) {
          payload.onEvent.onmessage({
            kind: "schema_event",
            data: {
              schema_version: 1,
              session_id: activeSid,
              run_id: "preview-run",
              seq: sequenceCounter++,
              timestamp: new Date().toISOString(),
              payload: {
                type: "model.text_delta",
                data: {
                  request_id: `steer_${Date.now()}`,
                  text: `\n> Steered: ${payload.prompt}\n`,
                },
              },
            },
          });
          return {
            action: "steered",
            session_id: activeSid,
            run_id: "preview-run",
          };
        }
        const abortToken = { cancelled: false };
        activeAbort = abortToken;
        lastChannel = payload.onEvent;
        simulateRunStream(payload.prompt, payload.onEvent, abortToken);
        return {
          action: "started",
          session_id: activeSid,
          run_id: "preview-run",
        };
      }
      case "cancel_run": {
        if (!activeAbort || activeAbort.cancelled) {
          return false;
        }
        activeAbort.cancelled = true;
        activeAbort = null;
        lastChannel?.onmessage({
          kind: "run_completed",
          data: {
            status: "cancelled",
            summary_line: "claude-sonnet-4-5 · cancelled",
            todos: CONVERSATION_SESSION.todos,
            error: null,
          },
        });
        return true;
      }
      case "compact_session": {
        const payload = args as {
          onEvent: Channel<DesktopStreamEnvelope_Deserialize>;
        };
        setTimeout(() => {
          payload.onEvent.onmessage({
            kind: "run_completed",
            data: {
              status: "completed",
              summary_line: "compacted · claude-sonnet-4-5 · 0.08s",
              todos: CONVERSATION_SESSION.todos,
              error: null,
            },
          });
        }, 80);
        return {
          action: "compacting",
          session_id: CONVERSATION_SESSION.id,
          run_id: `compact_${Date.now()}`,
        };
      }
      case "reply_approval":
        return null;
      case "reply_input":
        return null;
      case "set_work_mode": {
        const mode = (args as { mode: WorkModeDto }).mode;
        return {
          current: activeAbort ? CONVERSATION_SESSION.work_mode : mode,
          pending: activeAbort ? mode : null,
          active_run: Boolean(activeAbort),
        };
      }
      case "set_permission_mode":
        return (args as { permissions: PermissionModeDto }).permissions;
      case "resume_session": {
        const req = args as { sessionId: string; acceptConfigChange?: boolean };
        const chosen =
          req.sessionId === MARKDOWN_SHOWCASE_SESSION.id
            ? MARKDOWN_SHOWCASE_SESSION
            : CONVERSATION_SESSION;
        return {
          bootstrap: {
            ...buildBootstrap(
              req.sessionId === MARKDOWN_SHOWCASE_SESSION.id
                ? "markdown"
                : "conversation"
            ),
            session: chosen,
          },
          recovered_runs: 0,
          recovered_tools: 0,
        };
      }
      case "set_workspace": {
        previewWorkspace = (args as { path: string }).path;
        return currentBootstrapState();
      }
      case "select_profile": {
        previewProfile = (args as { profile: string }).profile;
        return currentBootstrapState();
      }
      case "set_thinking_effort": {
        previewEffort = (args as { effort: string | null }).effort;
        return currentBootstrapState();
      }
      case "set_sandbox_backend": {
        previewBackend = (args as { backend: SandboxBackendDto }).backend;
        return currentBootstrapState();
      }
      case "set_autocompact": {
        const payload = args as {
          enabled: boolean;
          triggerTokens: number | null;
        };
        previewAutoCompact = payload.enabled;
        previewTriggerTokens = payload.triggerTokens;
        return currentBootstrapState();
      }
      case "list_extensions":
        return [
          {
            kind: "skill",
            id: "skill:lucide-guidelines",
            name: "lucide-icons",
            description: "Workspace UI icon conventions using lucide-react",
            scope: "workspace",
            version: "1.0.0",
            active: true,
          },
        ];
      case "list_jobs":
        return [];
      case "cancel_background_job":
        return false;
      case "list_modules":
        return previewModules;
      case "set_module_enabled": {
        const payload = args as { module: string; enabled: boolean };
        previewModules = previewModules.map((m) =>
          m.module === payload.module
            ? {
                ...m,
                enabled: payload.enabled,
                active: payload.enabled,
                override_enabled: payload.enabled,
                source: "desktop_override",
              }
            : m
        );
        return previewModules;
      }
      case "list_mcp_servers":
        return previewMcpServers;
      case "toggle_mcp_server": {
        const payload = args as { name: string; enabled: boolean };
        previewMcpServers = previewMcpServers.map((s) =>
          s.name === payload.name ? { ...s, enabled: payload.enabled } : s
        );
        return previewMcpServers;
      }
      case "list_credentials":
        return previewCredentials;
      case "import_credential": {
        const payload = args as {
          server: string;
          envVar: string;
          scopes: string[];
          approveScopes: boolean;
        };
        const record: CredentialRecordDto_Serialize = {
          reference: `cred_${payload.server}_${Date.now()}`,
          server: payload.server,
          audience: `env://${payload.envVar}`,
          scopes: payload.scopes,
          kind: payload.approveScopes ? "bearer_env" : "pending_approval",
          revision: 1,
          revoked: false,
          oauth_resource: null,
        };
        previewCredentials = [...previewCredentials, record];
        return record;
      }
      case "revoke_credential": {
        const { reference } = args as { reference: string };
        previewCredentials = previewCredentials.map((c) =>
          c.reference === reference ? { ...c, revoked: true } : c
        );
        return true;
      }
      case "query_memory": {
        const { command } = args as { command: string };
        return {
          settings: { enabled: true, root: "~/.soyo/memory" },
          data: { command, entries: previewMemoryEntries },
        };
      }
      case "prepare_memory_action": {
        const { command } = args as { command: string };
        const actionId = `act_${Date.now()}`;
        pendingPreviewActions.set(actionId, command);
        return {
          action_id: actionId,
          operation_id: "op_preview_1",
          expected_revision: "1",
          review: { command, effect: "preview staged mutation" },
        };
      }
      case "confirm_memory_action": {
        const { actionId } = args as { actionId: string };
        const command = pendingPreviewActions.get(actionId) ?? "unknown";
        pendingPreviewActions.delete(actionId);
        return {
          result: { applied: true, action_id: actionId, command },
        };
      }
      case "export_session": {
        const payload = args as {
          targetPath: string;
          format: ExportFormatDto;
          scope: ExportScopeDto;
        };
        return {
          path: `${previewWorkspace}/${payload.targetPath}`,
          format: payload.format,
          scope: payload.scope,
          bytes: 2048,
          receipt: { verified: true, preview: true },
        };
      }
      case "stage_attachments": {
        const { paths } = args as { paths: string[] };
        return paths.map((p, idx): StagedAttachmentDto => {
          const filename = p.split("/").pop() || `attachment-${idx + 1}`;
          const isImg = /\.(png|jpe?g|webp|gif)$/i.test(filename);
          return {
            id: `draft_${Date.now()}_${idx}`,
            token: `tok_${Date.now()}_${idx}`,
            path: p,
            filename,
            mime: isImg ? "image/png" : "text/plain",
            size_bytes: 4096,
            kind: isImg ? "image" : "file",
            preview_url: `soyo-attachment://localhost/draft/draft_${idx}`,
          };
        });
      }
      case "get_usage_stats": {
        const req = args as { days?: number; utcOffsetMinutes?: number | null };
        const rangeDays = req.days ?? 30;
        const offsetSecs = (req.utcOffsetMinutes ?? 0) * 60;
        const daySec = 86400;
        const now = Math.floor(Date.now() / 1000);
        const shift = (ts: number) => ts + offsetSecs;
        const todayStart =
          shift(now) - ((shift(now) % daySec) + daySec) % daySec - offsetSecs;
        return {
          range_days: rangeDays,
          generated_at: now,
          totals: {
            input_tokens: 18420,
            output_tokens: 6140,
            cached_input_tokens: 4800,
            runs: 7,
            requests: 19,
            estimated: true,
          },
          by_purpose: [
            {
              purpose: "execution",
              totals: {
                input_tokens: 15200,
                output_tokens: 5100,
                cached_input_tokens: 4200,
                runs: 7,
                requests: 14,
                estimated: true,
              },
            },
            {
              purpose: "review",
              totals: {
                input_tokens: 2100,
                output_tokens: 640,
                cached_input_tokens: 400,
                runs: 3,
                requests: 3,
                estimated: false,
              },
            },
            {
              purpose: "compaction",
              totals: {
                input_tokens: 1120,
                output_tokens: 400,
                cached_input_tokens: 200,
                runs: 2,
                requests: 2,
                estimated: false,
              },
            },
          ],
          days: [
            {
              day_start: todayStart - 2 * daySec,
              input_tokens: 5200,
              output_tokens: 1800,
              cached_input_tokens: 1200,
              estimated: false,
            },
            {
              day_start: todayStart - daySec,
              input_tokens: 7100,
              output_tokens: 2340,
              cached_input_tokens: 1900,
              estimated: true,
            },
            {
              day_start: todayStart,
              input_tokens: 6120,
              output_tokens: 2000,
              cached_input_tokens: 1700,
              estimated: false,
            },
          ],
        };
      }
      case "get_helper_status":
        return {
          connected: true,
          version: "0.3.0",
          pid: 42100,
          active_runs: activeAbort && !activeAbort.cancelled ? 1 : 0,
          active_clients: 1,
          detail: null,
        };
      case "get_user_name":
        return "ariakage";
      case "get_desktop_prefs":
        return previewPrefs;
      case "update_desktop_prefs": {
        const req = (args as { params?: Partial<typeof previewPrefs> }).params ?? {};
        previewPrefs = {
          ...previewPrefs,
          ...(req.theme !== undefined ? { theme: req.theme } : {}),
          ...(req.font_size !== undefined ? { font_size: req.font_size } : {}),
          ...(req.conversation_font !== undefined
            ? { conversation_font: req.conversation_font }
            : {}),
          ...(req.reduce_motion !== undefined
            ? { reduce_motion: req.reduce_motion }
            : {}),
          ...(req.inspector_open !== undefined
            ? { inspector_open: req.inspector_open }
            : {}),
        };
        return previewPrefs;
      }
      case "set_nickname": {
        const req = args as { nickname?: string | null };
        previewPrefs = { ...previewPrefs, nickname: req.nickname ?? null };
        return previewPrefs;
      }
      case "set_avatar": {
        const req = args as { sourcePath?: string | null };
        previewPrefs = {
          ...previewPrefs,
          avatar_data: req.sourcePath ? "data:image/png;base64," : null,
        };
        return previewPrefs;
      }
      case "get_global_prompt":
        return {
          content: previewGlobalPrompt,
          path: "~/.soyo/AGENTS.md",
          source_path: null,
        };
      case "set_global_prompt": {
        const req = args as { content: string };
        previewGlobalPrompt = req.content;
        return {
          content: previewGlobalPrompt,
          path: "~/.soyo/AGENTS.md",
          source_path: null,
        };
      }
      default:
        throw {
          code: "preview_unimplemented",
          message: `Command '${cmd}' is not mocked in browser preview yet.`,
        };
    }
  });

  return true;
}
