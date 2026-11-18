import type { Channel } from "@tauri-apps/api/core";
import { mockIPC } from "@tauri-apps/api/mocks";
import type {
  ActiveSessionDto_Serialize,
  ApprovalRequestDto_Deserialize,
  BootstrapStateDto_Serialize,
  DesktopStreamEnvelope_Deserialize,
  InputRequestDto,
  PermissionModeDto,
  SessionPageDto_Serialize,
  WorkModeDto,
} from "../bindings";

export type PreviewScene =
  | "conversation"
  | "markdown"
  | "approval"
  | "clarification";

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
      param === "markdown" ||
      param === "conversation" ||
      param === "approval" ||
      param === "clarification"
    ) {
      currentScene = param;
    }
  }
  return currentScene;
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
    scene === "markdown" ? MARKDOWN_SHOWCASE_SESSION : CONVERSATION_SESSION;
  return {
    version: "0.2.0",
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
    ],
  };

  let lastChannel: Channel<DesktopStreamEnvelope_Deserialize> | null = null;

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
        return buildBootstrap(currentScene);
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
          onEvent: Channel<DesktopStreamEnvelope_Deserialize>;
        };
        if (activeAbort && !activeAbort.cancelled) {
          payload.onEvent.onmessage({
            kind: "schema_event",
            data: {
              schema_version: 1,
              session_id: CONVERSATION_SESSION.id,
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
            session_id: CONVERSATION_SESSION.id,
            run_id: "preview-run",
          };
        }
        const abortToken = { cancelled: false };
        activeAbort = abortToken;
        lastChannel = payload.onEvent;
        simulateRunStream(payload.prompt, payload.onEvent, abortToken);
        return {
          action: "started",
          session_id: CONVERSATION_SESSION.id,
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
      default:
        throw {
          code: "preview_unimplemented",
          message: `Command '${cmd}' is not mocked in browser preview yet.`,
        };
    }
  });

  return true;
}
