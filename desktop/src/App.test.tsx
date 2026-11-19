import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  BootstrapStateDto_Serialize,
  DesktopStreamEnvelope_Deserialize,
  SessionPageDto_Serialize,
} from "./bindings";

const { invokeMock, MockChannel, checkUpdateMock, isTauriMock } =
  vi.hoisted(() => {
    class HoistedChannel<T> {
      id = Math.floor(Math.random() * 100000);
      onmessage: (message: T) => void = () => {};
    }
    return {
      invokeMock: vi.fn(),
      MockChannel: HoistedChannel,
      checkUpdateMock: vi.fn(),
      isTauriMock: vi.fn(() => false),
    };
  });

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
  Channel: MockChannel,
  isTauri: () => isTauriMock(),
}));

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: () => Promise.resolve(() => {}),
  }),
}));

vi.mock("@tauri-apps/plugin-updater", () => ({
  check: (...args: unknown[]) => checkUpdateMock(...args),
}));

vi.mock("@streamdown/mermaid", () => ({
  createMermaidPlugin: () => ({
    name: "mermaid",
    type: "diagram",
    language: "mermaid",
    getMermaid: () => ({
      initialize: () => {},
      render: async () => ({ svg: "<svg></svg>" }),
    }),
  }),
}));

import App from "./App";
import { getMessages, greetingCandidates } from "./lib/i18n";
import { formatTokenCount } from "./lib/sanitize";
import { useAppStore } from "./store/useAppStore";

function sampleBootstrap(): BootstrapStateDto_Serialize {
  return {
    version: "0.3.0",
    config_ready: true,
    config_path: "/tmp/soyo/config.toml",
    config_error: null,
    workspace: "/workspace/demo",
    selected_profile: "default",
    selected_model: "claude-sonnet-4-5",
    context_window_tokens: 200000,
    thinking_effort: "medium",
    work_mode: "work",
    pending_mode: null,
    permission_mode: "auto",
    sandbox_backend: "seatbelt",
    auto_compact: true,
    auto_compact_trigger_tokens: 160000,
    auto_compact_trigger_percent: 80,
    profiles: [
      {
        name: "default",
        model: "claude-sonnet-4-5",
        protocol: "anthropic",
        context_window_tokens: 200000,
        thinking_effort: "medium",
      },
    ],
    session: {
      id: "01930000-0000-7000-8000-000000000001",
      title: "Inspect workspace",
      workspace: "/workspace/demo",
      work_mode: "work",
      profile: "default",
      model: "claude-sonnet-4-5",
      messages: [
        {
          id: "restored-1",
          role: "assistant",
          label: "Assistant",
          text: "## Restored Note\n\nExisting session context.",
          streaming: false,
          truncated: false,
          tool_call_id: null,
          tool_name: null,
          tool_summary: null,
          tool_ok: null,
          media_refs: [],
        },
      ],
      todos: [],
      history_omitted: false,
      last_usage: null,
    },
    run_active: false,
    active_run_id: null,
  };
}

function samplePage(): SessionPageDto_Serialize {
  return {
    page: 1,
    page_size: 20,
    items: [
      {
        id: "01930000-0000-7000-8000-000000000001",
        workspace: "/workspace/demo",
        work_mode: "work",
        profile: "default",
        title: "Inspect workspace",
        updated_at: 1700000000,
        last_status: "completed",
      },
    ],
  };
}

describe("App Stage 3 runtime integration", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    checkUpdateMock.mockReset();
    checkUpdateMock.mockResolvedValue(null);
    isTauriMock.mockReset();
    isTauriMock.mockReturnValue(false);
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      writable: true,
      value: 1280,
    });
    document.documentElement.dataset.theme = "dark";
    useAppStore.setState({
      theme: "system",
      resolvedTheme: "dark",
      fontSize: 14,
      conversationFont: "default",
      reduceMotion: false,
      inspectorOpenPref: null,
      helperReconnecting: false,
      initialized: false,
      loading: false,
      bootstrap: null,
      sessionId: null,
      sessionTitle: "New session",
      historyOmitted: false,
      workMode: "work",
      pendingMode: null,
      permissionMode: "auto",
      messages: [],
      thinkingEntries: [],
      contextEntries: [],
      contextSkillsCount: null,
      contextEstimated: true,
      mcpServers: [],
      todos: [],
      lastUsage: null,
      recentSessions: [],
      runActive: false,
      cancelling: false,
      activeRunId: null,
      summaryLine: null,
      markdownMode: "render",
      pendingApproval: null,
      pendingInput: null,
      stagedAttachments: [],
      ultracodeEnabled: false,
      submitMode: "steer",
      queuedPrompt: null,
      streamOwned: false,
      compactionPending: false,
      locale: "en-US",
      configChangeSessionId: null,
      recoveryBanner: null,
      error: null,
    });
    document.documentElement.lang = "en-US";
  });

  it("hydrates bootstrap state, streams prompt events via Channel, and cancels cleanly", async () => {
    let capturedChannel: {
      onmessage: (message: DesktopStreamEnvelope_Deserialize) => void;
    } | null = null;

    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "get_bootstrap_state") {
        return sampleBootstrap();
      }
      if (cmd === "get_user_name") {
        return "ariakage";
      }
      if (cmd === "get_desktop_prefs") {
        return { nickname: null, avatar_data: null };
      }
      if (cmd === "get_global_prompt") {
        return { content: "", path: "/tmp/AGENTS.md", source_path: null };
      }
      if (cmd === "list_sessions") {
        return samplePage();
      }
      if (cmd === "submit_prompt") {
        capturedChannel = args?.onEvent as typeof capturedChannel;
        setTimeout(() => {
          capturedChannel?.onmessage({
            kind: "thinking",
            data: [
              {
                request_id: "req_1",
                text: "Checking workspace layout",
                active: true,
                truncated: false,
                available: true,
              },
            ],
          });
          capturedChannel?.onmessage({
            kind: "schema_event",
            data: {
              schema_version: 1,
              session_id: "01930000-0000-7000-8000-000000000001",
              run_id: "run_1",
              seq: 1,
              timestamp: "2026-11-18T00:00:00Z",
              payload: {
                type: "model.text_delta",
                data: {
                  request_id: "req_1",
                  text: "Streaming answer ready.",
                },
              },
            },
          });
          capturedChannel?.onmessage({
            kind: "schema_event",
            data: {
              schema_version: 1,
              session_id: "01930000-0000-7000-8000-000000000001",
              run_id: "run_1",
              seq: 2,
              timestamp: "2026-11-18T00:00:01Z",
              payload: {
                type: "tool.requested",
                data: {
                  call_id: "call_1",
                  name: "read_file",
                  args: { path: "README.md" },
                  summary: "README.md",
                  formatted: "read_file README.md",
                },
              },
            },
          });
        }, 10);
        return {
          action: "started",
          session_id: "01930000-0000-7000-8000-000000000001",
          run_id: "run_1",
        };
      }
      if (cmd === "cancel_run") {
        capturedChannel?.onmessage({
          kind: "run_completed",
          data: {
            status: "cancelled",
            summary_line: "claude-sonnet-4-5 · cancelled",
            todos: [],
            error: null,
          },
        });
        return true;
      }
      return null;
    });

    render(<App />);

    await waitFor(() => {
      expect(screen.getAllByText("Inspect workspace").length).toBeGreaterThan(0);
      expect(screen.getByText(/Existing session context/)).toBeTruthy();
    });

    const input = screen.getByLabelText("Prompt input");
    fireEvent.change(input, { target: { value: "Stream test prompt" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => {
      expect(screen.getByText("Streaming answer ready.")).toBeTruthy();
      expect(screen.getByTestId("thinking-card")).toBeTruthy();
      expect(screen.getByTestId("tool-card")).toBeTruthy();
    });

    const stopButtons = screen.getAllByRole("button", { name: "Stop" });
    fireEvent.click(stopButtons[stopButtons.length - 1]);

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("cancel_run");
      expect(
        screen.getAllByText("claude-sonnet-4-5 · cancelled").length
      ).toBeGreaterThan(0);
    });

    // Global Markdown Source / Render toggle lives in the topbar overflow menu.
    fireEvent.click(screen.getByRole("button", { name: "More session actions" }));
    fireEvent.click(
      screen.getByRole("button", { name: "Show markdown source" })
    );
    expect(screen.getAllByTestId("markdown-source-view").length).toBeGreaterThan(0);
  });

  it("handles approval and clarification dialogs over Channel", async () => {
    let channel: {
      onmessage: (message: DesktopStreamEnvelope_Deserialize) => void;
    } | null = null;

    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "get_bootstrap_state") return sampleBootstrap();
      if (cmd === "list_sessions") return samplePage();
      if (cmd === "submit_prompt") {
        channel = args?.onEvent as typeof channel;
        setTimeout(() => {
          channel?.onmessage({
            kind: "approval_needed",
            data: {
              approval_id: "appr_1",
              call_id: "call_exec_1",
              run_id: "run_2",
              tool: "exec",
              operation_summary: "Run unit test suite",
              arguments: { command: "cargo test" },
              formatted_call: "exec $ cargo test",
              action: "shell.exec",
              cwd: "/workspace/demo",
              work_mode: "work",
            },
          });
        }, 10);
        return {
          action: "started",
          session_id: "01930000-0000-7000-8000-000000000001",
          run_id: "run_2",
        };
      }
      if (cmd === "reply_approval") {
        channel?.onmessage({
          kind: "input_needed",
          data: {
            input_id: "input_1",
            question: "Select deployment environment?",
            options: ["staging", "production"],
          },
        });
        return null;
      }
      if (cmd === "reply_input") {
        channel?.onmessage({
          kind: "run_completed",
          data: {
            status: "completed",
            summary_line: "claude-sonnet-4-5 · 0.20s",
            todos: [],
            error: null,
          },
        });
        return null;
      }
      return null;
    });

    render(<App />);
    await waitFor(() => {
      expect(screen.getAllByText("Inspect workspace").length).toBeGreaterThan(0);
    });

    const input = screen.getByLabelText("Prompt input");
    fireEvent.change(input, { target: { value: "Deploy check" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => {
      expect(screen.getByTestId("approval-dialog")).toBeTruthy();
    });
    fireEvent.click(screen.getByRole("button", { name: "Allow (Y)" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("reply_approval", {
        approvalId: "appr_1",
        callId: "call_exec_1",
        approved: true,
      });
      expect(screen.getByTestId("clarification-dialog")).toBeTruthy();
    });

    fireEvent.click(screen.getByRole("button", { name: /staging/i }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("reply_input", {
        inputId: "input_1",
        answer: "staging",
      });
      expect(screen.getAllByText("claude-sonnet-4-5 · 0.20s").length).toBeGreaterThan(0);
    });
  });

  it("switches work mode (including Tab cycle), permission mode, and resumes sessions with config change guard", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "get_bootstrap_state") return sampleBootstrap();
      if (cmd === "list_sessions") return samplePage();
      if (cmd === "set_work_mode") {
        const mode = (args as { mode: string }).mode;
        return { current: mode, pending: null, active_run: false };
      }
      if (cmd === "set_permission_mode") {
        return (args as { permissions: string }).permissions;
      }
      if (cmd === "resume_session") {
        const { sessionId, acceptConfigChange } = args as {
          sessionId: string;
          acceptConfigChange: boolean;
        };
        if (!acceptConfigChange) {
          throw {
            code: "config_change_requires_confirmation",
            message: "session configuration changed",
          };
        }
        return {
          bootstrap: {
            ...sampleBootstrap(),
            session: {
              ...sampleBootstrap().session!,
              id: sessionId,
              title: "Resumed Session",
            },
          },
          recovered_runs: 1,
          recovered_tools: 0,
        };
      }
      return null;
    });

    render(<App />);
    await waitFor(() => {
      expect(screen.getByTitle("Work mode")).toBeTruthy();
    });

    // Empty composer Tab cycles WORK -> CODE
    const input = screen.getByLabelText("Prompt input");
    fireEvent.keyDown(input, { key: "Tab" });
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_work_mode", { mode: "code" });
    });

    // Switch permission mode to manual via the topbar permission pill menu
    fireEvent.click(screen.getByTitle("Permission mode"));
    fireEvent.click(
      screen.getByRole("button", { name: /Manual approval/i })
    );
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_permission_mode", {
        permissions: "manual",
      });
    });

    // Open session browser and resume with config change confirmation
    fireEvent.click(screen.getByRole("button", { name: "Sessions" }));
    await waitFor(() => {
      expect(screen.getByTestId("session-browser-dialog")).toBeTruthy();
    });

    fireEvent.click(screen.getByRole("button", { name: "Current" }));
    await waitFor(() => {
      expect(
        screen.getByRole("button", { name: "Accept Config Change & Resume" })
      ).toBeTruthy();
    });

    fireEvent.click(
      screen.getByRole("button", { name: "Accept Config Change & Resume" })
    );
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("resume_session", {
        sessionId: "01930000-0000-7000-8000-000000000001",
        acceptConfigChange: true,
      });
      expect(screen.getAllByText("Resumed Session").length).toBeGreaterThan(0);
    });
  });

  it("opens settings, toggles modules, manages MCP/credentials metadata, and runs two-phase memory admin", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "get_bootstrap_state") return sampleBootstrap();
      if (cmd === "list_sessions") return samplePage();
      if (cmd === "set_workspace") {
        return {
          ...sampleBootstrap(),
          workspace: (args as { path: string }).path,
        };
      }
      if (cmd === "list_modules") {
        return [
          {
            module: "mcp",
            description: "Managed MCP bridge",
            enabled: true,
            configured_enabled: true,
            override_enabled: null,
            source: "config",
            active: true,
            issue: null,
          },
        ];
      }
      if (cmd === "set_module_enabled") {
        const { module, enabled } = args as { module: string; enabled: boolean };
        return [
          {
            module,
            description: "Managed MCP bridge",
            enabled,
            configured_enabled: true,
            override_enabled: enabled,
            source: "desktop_override",
            active: enabled,
            issue: null,
          },
        ];
      }
      if (cmd === "list_extensions") return [];
      if (cmd === "list_jobs") return [];
      if (cmd === "list_mcp_servers") {
        return [
          {
            name: "docs-http",
            transport: "http",
            enabled: true,
            target: "https://docs.example.internal/mcp",
            scopes: ["docs:read"],
            credential_ref: "cred_ref_1",
          },
        ];
      }
      if (cmd === "list_credentials") {
        return [
          {
            reference: "cred_ref_1",
            server: "docs-http",
            audience: "https://docs.example.internal",
            scopes: ["docs:read"],
            kind: "bearer_env",
            revision: 1,
            revoked: false,
            oauth_resource: null,
          },
        ];
      }
      if (cmd === "import_credential") {
        const { server, envVar, scopes } = args as {
          server: string;
          envVar: string;
          scopes: string[];
        };
        return {
          reference: `cred_${server}_2`,
          server,
          audience: `env://${envVar}`,
          scopes,
          kind: "bearer_env",
          revision: 1,
          revoked: false,
          oauth_resource: null,
        };
      }
      if (cmd === "query_memory") {
        return {
          settings: { enabled: true },
          data: { entries: [{ id: "fact_1", text: "Lucide icons required" }] },
        };
      }
      if (cmd === "prepare_memory_action") {
        return {
          action_id: "mem_act_1",
          operation_id: "op_1",
          expected_revision: "rev_1",
          review: { target: "fact_1", action: "forget" },
        };
      }
      if (cmd === "confirm_memory_action") {
        return { result: { applied: true } };
      }
      return null;
    });

    render(<App />);
    await waitFor(() => {
      expect(screen.getAllByText("Inspect workspace").length).toBeGreaterThan(0);
    });

    // Settings + Modules
    fireEvent.click(screen.getByRole("button", { name: "Customize" }));
    await waitFor(() => {
      expect(screen.getByTestId("settings-dialog")).toBeTruthy();
    });

    const settingsDialog = screen.getByTestId("settings-dialog");
    fireEvent.click(within(settingsDialog).getByRole("button", { name: "General" }));
    const wsInput = screen.getByLabelText("Workspace Directory");
    fireEvent.change(wsInput, { target: { value: "/workspace/updated" } });
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_workspace", {
        path: "/workspace/updated",
      });
    });

    fireEvent.click(within(settingsDialog).getByRole("button", { name: "Modules" }));
    await waitFor(() => {
      expect(screen.getByTestId("modules-panel")).toBeTruthy();
    });
    fireEvent.click(screen.getByRole("button", { name: "Enabled" }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_module_enabled", {
        module: "mcp",
        enabled: false,
      });
    });

    // MCP & Credentials (embedded tab inside Settings)
    fireEvent.click(
      within(settingsDialog).getByRole("button", { name: "MCP & Credentials" })
    );
    await waitFor(() => {
      expect(screen.getByTestId("mcp-embedded-panel")).toBeTruthy();
      expect(screen.getByText(/docs-http \(bearer_env\)/)).toBeTruthy();
    });

    fireEvent.change(screen.getByLabelText("Environment variable name"), {
      target: { value: "DOCS_BEARER_TOKEN" },
    });
    fireEvent.change(screen.getByLabelText("Credential scopes"), {
      target: { value: "docs:read" },
    });
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: /Explicitly approve endpoint & scopes/i,
      })
    );
    fireEvent.click(screen.getByRole("button", { name: "Import Reference" }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("import_credential", {
        server: "docs-http",
        envVar: "DOCS_BEARER_TOKEN",
        scopes: ["docs:read"],
        approveScopes: true,
      });
    });

    // Memory two-phase review/confirm (embedded tab inside Settings)
    fireEvent.click(within(settingsDialog).getByRole("button", { name: "Memory" }));
    await waitFor(() => {
      expect(screen.getByTestId("memory-embedded-panel")).toBeTruthy();
      expect(invokeMock).toHaveBeenCalledWith("query_memory", { command: "list" });
    });

    fireEvent.change(screen.getByLabelText("Memory action command"), {
      target: { value: "forget fact_1" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Prepare Review" }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("prepare_memory_action", {
        command: "forget fact_1",
      });
      expect(screen.getByRole("button", { name: "Confirm Apply" })).toBeTruthy();
    });

    fireEvent.click(screen.getByRole("button", { name: "Confirm Apply" }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("confirm_memory_action", {
        actionId: "mem_act_1",
      });
    });
    fireEvent.click(screen.getByLabelText("Close settings"));
  });

  it("exports sessions, stages composer attachments, and renders session media previews", async () => {
    const boot = sampleBootstrap();
    boot.session!.messages = [
      ...boot.session!.messages,
      {
        id: "tool-media-1",
        role: "tool",
        label: "browser_screenshot",
        text: "Captured viewport png",
        streaming: false,
        truncated: false,
        tool_call_id: "call_shot_1",
        tool_name: "browser_screenshot",
        tool_summary: "shot.png",
        tool_ok: true,
        media_refs: ["shot-1.png"],
      },
    ];

    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "get_bootstrap_state") return boot;
      if (cmd === "list_sessions") return samplePage();
      if (cmd === "export_session") {
        return {
          path: "/workspace/demo/session-export.md",
          format: (args as { format: string }).format,
          scope: (args as { scope: string }).scope,
          bytes: 512,
          receipt: { verified: true },
        };
      }
      if (cmd === "stage_attachments") {
        return [
          {
            id: "draft_img_1",
            token: "att_tok_1",
            path: "/workspace/demo/preview.png",
            filename: "preview.png",
            mime: "image/png",
            size_bytes: 2048,
            kind: "image",
            preview_url: "soyo-attachment://localhost/draft/draft_img_1",
          },
        ];
      }
      if (cmd === "submit_prompt") {
        return {
          action: "started",
          session_id: "01930000-0000-7000-8000-000000000001",
          run_id: "run_att_1",
        };
      }
      return null;
    });

    render(<App />);
    await waitFor(() => {
      expect(screen.getByTestId("session-media-preview")).toBeTruthy();
    });
    const sessionMediaImg = screen
      .getByTestId("session-media-preview")
      .querySelector("img");
    expect(sessionMediaImg?.getAttribute("src")).toContain(
      "soyo-attachment://localhost/media/"
    );

    // Export session
    fireEvent.click(screen.getByRole("button", { name: "More session actions" }));
    fireEvent.click(
      screen.getByRole("button", { name: "Export session…" })
    );
    await waitFor(() => {
      expect(screen.getByTestId("export-dialog")).toBeTruthy();
    });
    fireEvent.click(screen.getByRole("button", { name: "Publish Export" }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("export_session", {
        sessionId: "01930000-0000-7000-8000-000000000001",
        targetPath: "session-export.md",
        format: "markdown",
        scope: "transcript",
        includeTools: true,
      });
      expect(screen.getByText("Export published")).toBeTruthy();
    });
    fireEvent.click(screen.getByLabelText("Close export dialog"));

    // Stage attachment via + context menu & submit
    fireEvent.click(screen.getByRole("button", { name: "Add context menu" }));
    fireEvent.click(
      screen.getByRole("button", { name: "Enter path manually" })
    );
    fireEvent.change(screen.getByLabelText("Local attachment file path"), {
      target: { value: "/workspace/demo/preview.png" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Stage" }));
    await waitFor(() => {
      expect(screen.getByTestId("staged-attachments-strip")).toBeTruthy();
      expect(screen.getByText("preview.png")).toBeTruthy();
    });

    fireEvent.change(screen.getByLabelText("Prompt input"), {
      target: { value: "Inspect attached image" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "submit_prompt",
        expect.objectContaining({
          prompt: "Inspect attached image",
          attachments: [
            {
              token: "att_tok_1",
              path: "/workspace/demo/preview.png",
            },
          ],
        })
      );
    });
  });

  it("displays updater banner when update exists and triggers downloadAndInstall on user confirmation", async () => {
    const downloadAndInstall = vi.fn().mockResolvedValue(undefined);
    const close = vi.fn().mockResolvedValue(undefined);
    checkUpdateMock.mockResolvedValue({
      available: true,
      currentVersion: "0.2.0",
      version: "0.3.1",
      rawJson: {},
      downloadAndInstall,
      close,
    });

    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_bootstrap_state") return sampleBootstrap();
      if (cmd === "list_sessions") return samplePage();
      return null;
    });

    (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    try {
      render(<App />);
      await waitFor(() => {
        expect(screen.getByTestId("updater-banner")).toBeTruthy();
        expect(screen.getByText(/Update available: v0\.3\.1/)).toBeTruthy();
      });

      fireEvent.click(screen.getByRole("button", { name: "Install & Restart" }));
      await waitFor(() => {
        expect(downloadAndInstall).toHaveBeenCalledTimes(1);
        expect(
          screen.getByText(/Update installed — restart Soyo to finish applying\./)
        ).toBeTruthy();
      });

      fireEvent.click(screen.getByLabelText("Dismiss update notification"));
      await waitFor(() => {
        expect(screen.queryByTestId("updater-banner")).toBeNull();
      });
    } finally {
      delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
    }
  });

  it("opens EffortPopover slider, toggles Ultracode, supports slash palette and Usage tab", async () => {
    let currentEffort: string | null = "medium";
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "get_bootstrap_state") {
        return {
          ...sampleBootstrap(),
          thinking_effort: currentEffort,
        };
      }
      if (cmd === "list_sessions") return samplePage();
      if (cmd === "list_modules") return [];
      if (cmd === "list_extensions") return [];
      if (cmd === "list_jobs") return [];
      if (cmd === "set_thinking_effort") {
        currentEffort = (args as { effort: string | null }).effort;
        return {
          ...sampleBootstrap(),
          thinking_effort: currentEffort,
        };
      }
      return null;
    });

    render(<App />);
    await waitFor(() => {
      expect(screen.getByTestId("effort-pill")).toBeTruthy();
      expect(screen.getByTestId("effort-pill").textContent).toContain("Effort: Medium");
    });

    // Open cc_eff Effort Popover and toggle Ultracode
    fireEvent.click(screen.getByTestId("effort-pill"));
    await waitFor(() => {
      expect(screen.getByTestId("effort-popover")).toBeTruthy();
      expect(screen.getByTestId("effort-level-label").textContent).toBe("Medium");
    });

    fireEvent.click(screen.getByRole("switch", { name: "Ultracode toggle" }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_thinking_effort", { effort: "max" });
      expect(screen.getByTestId("effort-level-label").textContent).toBe("Ultracode");
      expect(screen.getByTestId("effort-pill").textContent).toContain("Effort: Ultracode");
    });

    // Keyboard step left from Ultracode -> Max (with High usage warning)
    fireEvent.keyDown(screen.getByRole("slider", { name: "Effort slider" }), {
      key: "ArrowLeft",
    });
    await waitFor(() => {
      expect(screen.getByTestId("effort-level-label").textContent).toBe("Max");
      expect(
        screen.getByText(/Model thinks longer with Max effort and uses your limits faster/i)
      ).toBeTruthy();
    });

    // Slash command palette
    const input = screen.getByLabelText("Prompt input");
    fireEvent.change(input, { target: { value: "/mo" } });
    await waitFor(() => {
      expect(screen.getByTestId("slash-command-palette")).toBeTruthy();
    });

    // Settings Usage tab
    fireEvent.click(screen.getByRole("button", { name: "Customize" }));
    await waitFor(() => {
      expect(screen.getByTestId("settings-dialog")).toBeTruthy();
    });
    fireEvent.click(screen.getByRole("button", { name: "Usage" }));
    await waitFor(() => {
      expect(screen.getByTestId("usage-panel")).toBeTruthy();
      expect(
        screen.getByText("No recorded usage in this window")
      ).toBeTruthy();
    });

    // Escape closes SettingsDialog
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => {
      expect(screen.queryByTestId("settings-dialog")).toBeNull();
    });
  });

  it("loads backend usage stats, renders purpose totals and day chart, and refetches on range change", async () => {
    isTauriMock.mockReturnValue(true);
    let requestedDays: number[] = [];
    const usageStats = (rangeDays: number) => ({
      range_days: rangeDays,
      generated_at: 1_700_000_000,
      totals: {
        input_tokens: 10_000,
        output_tokens: 5_000,
        cached_input_tokens: 2_000,
        runs: 2,
        requests: 4,
        estimated: true,
      },
      by_purpose: [
        {
          purpose: "execution",
          totals: {
            input_tokens: 8_000,
            output_tokens: 4_000,
            cached_input_tokens: 1_500,
            runs: 2,
            requests: 3,
            estimated: true,
          },
        },
        {
          purpose: "review",
          totals: {
            input_tokens: 1_500,
            output_tokens: 800,
            cached_input_tokens: 400,
            runs: 1,
            requests: 1,
            estimated: false,
          },
        },
        {
          purpose: "compaction",
          totals: {
            input_tokens: 500,
            output_tokens: 200,
            cached_input_tokens: 100,
            runs: 1,
            requests: 1,
            estimated: false,
          },
        },
      ],
      days: [
        {
          day_start: 1_699_948_800,
          input_tokens: 6_000,
          output_tokens: 3_000,
          cached_input_tokens: 1_200,
          estimated: true,
        },
        {
          day_start: 1_700_035_200,
          input_tokens: 4_000,
          output_tokens: 2_000,
          cached_input_tokens: 800,
          estimated: false,
        },
      ],
    });
    invokeMock.mockImplementation(
      async (cmd: string, args?: Record<string, unknown>) => {
        if (cmd === "get_bootstrap_state") {
          return sampleBootstrap();
        }
        if (cmd === "get_usage_stats") {
          const days = Number(args?.days ?? 0);
          requestedDays.push(days);
          return usageStats(days);
        }
        if (cmd === "get_global_prompt") {
          return { content: "", path: "/tmp/soyo/prompt.md" };
        }
        return null;
      }
    );

    render(<App />);
    await waitFor(() => {
      expect(screen.getByLabelText("Prompt input")).toBeTruthy();
    });

    fireEvent.click(screen.getByRole("button", { name: "Customize" }));
    await waitFor(() => {
      expect(screen.getByTestId("settings-dialog")).toBeTruthy();
    });
    fireEvent.click(screen.getByRole("button", { name: "Usage" }));
    await waitFor(() => {
      expect(screen.getByTestId("usage-panel")).toBeTruthy();
    });
    await waitFor(() => {
      expect(requestedDays).toEqual([30]);
      expect(screen.getByText("Execution")).toBeTruthy();
      expect(screen.getByText("Review")).toBeTruthy();
      expect(screen.getByText("Compaction")).toBeTruthy();
      expect(screen.getByText("Tokens per day")).toBeTruthy();
      expect(screen.getByText(/Includes estimated values/)).toBeTruthy();
    });
    // Estimated execution total: ~12.0k input+output.
    expect(screen.getByText(/~12\.0k/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "7d" }));
    await waitFor(() => {
      expect(requestedDays).toEqual([30, 7]);
    });
  });

  it("renders inline diff preview, subagent nested steps, and ContextSidebar outputs", async () => {
    const boot = sampleBootstrap();
    boot.session!.messages = [
      ...boot.session!.messages,
      {
        id: "tool-edit-diff",
        role: "tool",
        label: "Tool · edit_file",
        text: [
          "--- a/desktop/src/App.tsx",
          "+++ b/desktop/src/App.tsx",
          "@@ -10,3 +10,4 @@ export function App()",
          "-const legacy = true;",
          "+const legacy = false;",
          "+const refined = true;",
        ].join("\n"),
        streaming: false,
        truncated: false,
        tool_call_id: "call_edit_diff_1",
        tool_name: "edit_file",
        tool_summary: "desktop/src/App.tsx",
        tool_ok: true,
        media_refs: [],
      },
      {
        id: "tool-subagent-1",
        role: "tool",
        label: "subagent_explore",
        text: "grep restore_session -> done\nread_file session_store.rs -> running",
        streaming: false,
        truncated: false,
        tool_call_id: "call_sub_nested_1",
        tool_name: "subagent_explore",
        tool_summary: "2 tool calls",
        tool_ok: true,
        media_refs: [],
      },
    ];

    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_bootstrap_state") return boot;
      if (cmd === "list_sessions") return samplePage();
      return null;
    });

    render(<App />);

    await waitFor(() => {
      expect(screen.getByTestId("inline-diff-card")).toBeTruthy();
      expect(screen.getAllByText("+2").length).toBeGreaterThan(0);
      expect(screen.getAllByText("-1").length).toBeGreaterThan(0);
      expect(screen.getByText(/1 hunk/i)).toBeTruthy();
      expect(screen.getByText("restore_session")).toBeTruthy();
      expect(screen.getByText("session_store.rs")).toBeTruthy();
      expect(screen.getByTestId("context-sidebar").textContent).toContain(
        "desktop/src/App.tsx"
      );
    });
  });

  it("keeps single collapsible sidebar expanded on Home, exposes unified titlebar controls, and formats large token counts", async () => {
    expect(formatTokenCount(850)).toBe("850");
    expect(formatTokenCount(12_400)).toBe("12.4k");
    expect(formatTokenCount(152_300_000)).toBe("152.3M");
    expect(formatTokenCount(2_500_000_000)).toBe("2.5B");

    const boot = sampleBootstrap();
    boot.session!.messages = [];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_bootstrap_state") return boot;
      if (cmd === "list_sessions") return samplePage();
      return null;
    });

    render(<App />);

    await waitFor(() => {
      expect(screen.getByTestId("home-empty-state")).toBeTruthy();
    });

    // D-075: Single 248px sidebar stays expanded on Home and collapses to 48px rail on toggle
    const sidebar = screen.getByTestId("sidebar-panel");
    expect(sidebar.className).toContain("w-[248px]");
    expect(screen.getByRole("button", { name: "User menu" })).toBeTruthy();

    // Titlebar toggle collapses sidebar to 48px icon rail and restores it back
    fireEvent.click(screen.getByRole("button", { name: "Toggle sidebar" }));
    await waitFor(() => {
      expect(screen.getByTestId("sidebar-panel").className).toContain("w-[48px]");
    });

    fireEvent.click(screen.getByRole("button", { name: "Toggle sidebar" }));
    await waitFor(() => {
      expect(screen.getByTestId("sidebar-panel").className).toContain("w-[248px]");
    });

    // Segmented markdown view switcher in the unified titlebar updates store state
    fireEvent.click(screen.getByRole("button", { name: "Markdown source view" }));
    expect(useAppStore.getState().markdownMode).toBe("source");
    fireEvent.click(screen.getByRole("button", { name: "Rendered conversation view" }));
    expect(useAppStore.getState().markdownMode).toBe("render");
  });

  it("opens Choose your language from user menu and switches en-US, zh-CN, and zh-TW", async () => {
    const boot = sampleBootstrap();
    boot.session!.messages = [];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_bootstrap_state") return boot;
      if (cmd === "get_user_name") return "ariakage";
      if (cmd === "get_desktop_prefs") {
        return { nickname: null, avatar_data: null };
      }
      if (cmd === "list_sessions") return samplePage();
      if (cmd === "list_modules" || cmd === "list_extensions" || cmd === "list_jobs") {
        return [];
      }
      if (cmd === "list_mcp_servers" || cmd === "list_credentials") {
        return [];
      }
      if (cmd === "query_memory") {
        return { settings: { enabled: true }, data: { entries: [] } };
      }
      return null;
    });

    const expectGreetingFrom = (locale: "en-US" | "zh-CN" | "zh-TW") => {
      const candidates = greetingCandidates(getMessages(locale).home, "ariakage");
      const shown = candidates.some((g) => screen.queryByText(g) !== null);
      expect(shown).toBeTruthy();
    };

    render(<App />);

    await waitFor(() => {
      expectGreetingFrom("en-US");
    });

    // Open user menu and verify Language entry
    fireEvent.click(screen.getByRole("button", { name: "User menu" }));
    const langEntry = await screen.findByTestId("user-menu-language");
    fireEvent.click(langEntry);

    await waitFor(() => {
      expect(screen.getByTestId("language-dialog")).toBeTruthy();
      expect(screen.getByText("Choose your language")).toBeTruthy();
      expect(screen.getByTestId("language-option-en-US")).toBeTruthy();
      expect(screen.getByTestId("language-option-zh-CN")).toBeTruthy();
      expect(screen.getByTestId("language-option-zh-TW")).toBeTruthy();
    });

    // Switch to Simplified Chinese
    fireEvent.click(screen.getByTestId("language-option-zh-CN"));
    await waitFor(() => {
      expect(useAppStore.getState().locale).toBe("zh-CN");
      expect(document.documentElement.lang).toBe("zh-CN");
      expect(screen.getByText("选择语言")).toBeTruthy();
      expectGreetingFrom("zh-CN");
    });

    // Escape closes LanguageDialog, verify dialogs localize cleanly in zh-CN
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => {
      expect(screen.queryByTestId("language-dialog")).toBeNull();
    });

    fireEvent.click(screen.getByRole("button", { name: "自定义" }));
    await waitFor(() => {
      expect(screen.getByTestId("settings-dialog")).toBeTruthy();
      expect(screen.getByText("界面语言")).toBeTruthy();
    });
    const zhSettings = screen.getByTestId("settings-dialog");
    fireEvent.click(within(zhSettings).getByRole("button", { name: "常规" }));
    await waitFor(() => {
      expect(screen.getByText("工作区目录")).toBeTruthy();
    });
    fireEvent.click(within(zhSettings).getByRole("button", { name: "MCP 与凭据" }));
    await waitFor(() => {
      expect(screen.getByTestId("mcp-embedded-panel")).toBeTruthy();
      expect(screen.getByText("已配置的 MCP 服务器")).toBeTruthy();
    });
    fireEvent.click(within(zhSettings).getByRole("button", { name: "记忆" }));
    await waitFor(() => {
      expect(screen.getByTestId("memory-embedded-panel")).toBeTruthy();
    });
    fireEvent.click(screen.getByLabelText("关闭设置"));

    fireEvent.click(screen.getByRole("button", { name: "会话" }));
    await waitFor(() => {
      expect(screen.getByTestId("session-browser-dialog")).toBeTruthy();
      expect(screen.getByText("会话与项目历史")).toBeTruthy();
      expect(screen.getByText("会话详情")).toBeTruthy();
    });
    fireEvent.click(screen.getByLabelText("关闭会话浏览器"));

    fireEvent.click(screen.getByRole("button", { name: "导出" }));
    await waitFor(() => {
      expect(screen.getByTestId("export-dialog")).toBeTruthy();
      expect(screen.getByText("导出会话记录")).toBeTruthy();
    });
    fireEvent.click(screen.getByLabelText("关闭导出对话框"));

    // Switch to Traditional Chinese and verify LanguageDialog + SettingsDialog
    fireEvent.click(screen.getByRole("button", { name: "用户菜单" }));
    fireEvent.click(await screen.findByTestId("user-menu-language"));
    fireEvent.click(await screen.findByTestId("language-option-zh-TW"));
    await waitFor(() => {
      expect(useAppStore.getState().locale).toBe("zh-TW");
      expect(document.documentElement.lang).toBe("zh-TW");
      expect(screen.getByText("選擇語言")).toBeTruthy();
      expectGreetingFrom("zh-TW");
    });

    // Switch back to English (United States)
    fireEvent.click(screen.getByTestId("language-option-en-US"));
    await waitFor(() => {
      expect(useAppStore.getState().locale).toBe("en-US");
      expect(screen.getByText("Choose your language")).toBeTruthy();
      expectGreetingFrom("en-US");
    });

    // Escape closes LanguageDialog
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => {
      expect(screen.queryByTestId("language-dialog")).toBeNull();
    });
  });

  it("renders context ring + estimated usage, replays queued prompts after pending compaction", async () => {
    const sid = "01930000-0000-7000-8000-000000000001";
    const submitted: string[] = [];
    let channel: {
      onmessage: (message: DesktopStreamEnvelope_Deserialize) => void;
    } | null = null;

    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "get_bootstrap_state") return sampleBootstrap();
      if (cmd === "list_sessions") return samplePage();
      if (cmd === "submit_prompt") {
        const prompt = (args as { prompt: string }).prompt;
        submitted.push(prompt);
        channel = args?.onEvent as typeof channel;
        return { action: "started", session_id: sid, run_id: `run_${submitted.length}` };
      }
      if (cmd === "compact_session") {
        return { action: "compacting", session_id: sid, run_id: "run_compact" };
      }
      return null;
    });

    render(<App />);
    await waitFor(() => {
      expect(screen.getByText(/Existing session context/)).toBeTruthy();
    });

    // First prompt starts an owned run.
    const input = screen.getByLabelText("Prompt input");
    fireEvent.change(input, { target: { value: "first" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => {
      expect(submitted).toEqual(["first"]);
      expect(useAppStore.getState().runActive).toBe(true);
      expect(useAppStore.getState().streamOwned).toBe(true);
    });

    // Context envelope: 60k of 200k window -> ring at 30%.
    act(() => {
      useAppStore.getState().handleStreamEnvelope({
        kind: "context",
        data: {
          entries: [
            { label: "system", tokens: 40000 },
            { label: "history", tokens: 20000 },
          ],
          skills_count: 2,
          estimated: true,
        },
      });
      // Estimated usage from model.finished.
      useAppStore.getState().handleStreamEnvelope({
        kind: "schema_event",
        data: {
          schema_version: 1,
          session_id: sid,
          run_id: "run_1",
          seq: 1,
          timestamp: "2026-11-18T00:00:00Z",
          payload: {
            type: "model.finished",
            data: {
              request_id: "req_1",
              purpose: "execution",
              ok: true,
              error: null,
              usage: {
                input_tokens: 61000,
                output_tokens: 800,
                cached_input_tokens: null,
                estimated: true,
              },
            },
          },
        },
      });
    });
    await waitFor(() => {
      expect(
        screen.getByRole("img", { name: "Context usage 30%" })
      ).toBeTruthy();
      const usage = screen.getByTitle(/Includes estimated values/i);
      expect(usage.textContent).toContain("~");
      expect(usage.textContent).toContain("61.0k");
    });

    // Queue mode holds the next prompt locally instead of steering.
    fireEvent.click(screen.getByRole("button", { name: "Steer" }));
    fireEvent.change(input, { target: { value: "second" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => {
      expect(useAppStore.getState().queuedPrompt?.prompt).toBe("second");
      expect(submitted).toEqual(["first"]);
      expect(screen.getByText("queued")).toBeTruthy();
    });

    // Run completes with a still-pending compaction: the follow-up compact
    // fires first and the queued prompt is NOT flushed yet.
    act(() => {
      useAppStore.getState().handleStreamEnvelope({
        kind: "run_completed",
        data: {
          status: "completed",
          summary_line: "claude-sonnet-4-5 · done",
          todos: [],
          error: null,
          compaction_pending: true,
        },
      });
    });
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith(
        "compact_session",
        expect.anything()
      );
      expect(submitted).toEqual(["first"]);
      expect(useAppStore.getState().queuedPrompt?.prompt).toBe("second");
    });

    // Once the compaction run settles, the queued prompt replays.
    act(() => {
      useAppStore.getState().handleStreamEnvelope({
        kind: "run_completed",
        data: {
          status: "completed",
          summary_line: "compacted",
          todos: [],
          error: null,
          compaction_pending: false,
          needs_input_question: "Which region should deploy?",
        },
      });
    });
    // The needs-input question lands in the summary line before the queued
    // resubmission clears it on the next tick.
    expect(
      useAppStore.getState().summaryLine?.includes("Which region")
    ).toBe(true);
    await waitFor(() => {
      expect(submitted).toEqual(["first", "second"]);
      expect(useAppStore.getState().queuedPrompt).toBeNull();
    });
  });

  it("polls run state for runs owned by another client and reconciles on finish", async () => {
    const boot = sampleBootstrap();
    boot.run_active = true;
    boot.active_run_id = "run_foreign";

    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_bootstrap_state") return boot;
      if (cmd === "list_sessions") return samplePage();
      if (cmd === "get_run_state") {
        // Simulate the helper having finished: bootstrap refresh agrees.
        boot.run_active = false;
        boot.active_run_id = null;
        boot.work_mode = "code";
        boot.session!.work_mode = "code";
        return {
          current: "code",
          pending: null,
          active_run: false,
          compaction_pending: false,
        };
      }
      return null;
    });

    render(<App />);
    await waitFor(() => {
      expect(useAppStore.getState().runActive).toBe(true);
      expect(useAppStore.getState().streamOwned).toBe(false);
    });

    // Foreign run finished: polling sync picks it up and reconciles bootstrap.
    await act(async () => {
      await useAppStore.getState().syncRunState();
    });
    await waitFor(() => {
      expect(useAppStore.getState().runActive).toBe(false);
      expect(useAppStore.getState().workMode).toBe("code");
    });
  });

  it("gates first-run setup behind the wizard and applies the returned bootstrap", async () => {
    isTauriMock.mockReturnValue(true);
    const incomplete = {
      ...sampleBootstrap(),
      config_ready: false,
      config_error: "cannot read selected configuration file",
      profiles: [],
    };
    const ready = sampleBootstrap();
    let setupParams: Record<string, unknown> | null = null;
    invokeMock.mockImplementation(
      async (cmd: string, args?: Record<string, unknown>) => {
        if (cmd === "get_bootstrap_state") return incomplete;
        if (cmd === "get_user_name") return "ariakage";
        if (cmd === "get_desktop_prefs")
          return { nickname: null, avatar_data: null };
        if (cmd === "get_global_prompt")
          return { content: "", path: "/tmp/AGENTS.md", source_path: null };
        if (cmd === "list_sessions") return samplePage();
        if (cmd === "setup_environment") {
          setupParams = args?.params as Record<string, unknown>;
          return ready;
        }
        return null;
      }
    );

    render(<App />);
    const wizard = await screen.findByTestId("setup-wizard");
    // The config error that triggered first-run setup is surfaced up front.
    expect(wizard.textContent).toContain(
      "cannot read selected configuration file"
    );

    // Step 1: language — continue with the default.
    fireEvent.click(screen.getByText("Continue"));
    // Step 2: workspace — prefilled from bootstrap.workspace.
    await screen.findByTestId("setup-step-workspace");
    expect(
      (screen.getByLabelText("Workspace directory") as HTMLInputElement).value
    ).toBe("/workspace/demo");
    fireEvent.click(screen.getByText("Continue"));

    // Step 3: model — fill the required model + API key fields.
    await screen.findByTestId("setup-step-model");
    fireEvent.change(
      screen.getByPlaceholderText("gpt-5 / claude-sonnet-4-5 / …"),
      { target: { value: "gpt-5-mini" } }
    );
    fireEvent.change(screen.getByLabelText("API key"), {
      target: { value: "sk-wizard-1" },
    });
    fireEvent.click(screen.getByText("Continue"));

    // Step 4: permissions — finish with the default auto mode.
    await screen.findByTestId("setup-step-permissions");
    fireEvent.click(screen.getByText("Finish setup"));

    await waitFor(() => {
      expect(setupParams).not.toBeNull();
    });
    expect(setupParams).toMatchObject({
      profile_name: "openai",
      protocol: "openai-responses",
      base_url: "https://api.openai.com/v1",
      model: "gpt-5-mini",
      auth: "bearer",
      api_key_env: "OPENAI_API_KEY",
      api_key: "sk-wizard-1",
      context_window_tokens: 128000,
      workspace: "/workspace/demo",
      permission_mode: "auto",
    });

    // Returned bootstrap is applied: the wizard closes into the normal app.
    await waitFor(() => {
      expect(screen.queryByTestId("setup-wizard")).toBeNull();
      expect(useAppStore.getState().bootstrap?.config_ready).toBe(true);
    });
  });

  it("surfaces setup backend errors and can skip to the config warning", async () => {
    isTauriMock.mockReturnValue(true);
    const incomplete = {
      ...sampleBootstrap(),
      config_ready: false,
      config_error: null,
      profiles: [],
    };
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_bootstrap_state") return incomplete;
      if (cmd === "get_user_name") return "ariakage";
      if (cmd === "get_desktop_prefs")
        return { nickname: null, avatar_data: null };
      if (cmd === "get_global_prompt")
        return { content: "", path: "/tmp/AGENTS.md", source_path: null };
      if (cmd === "list_sessions") return samplePage();
      if (cmd === "setup_environment")
        throw { code: "config_io", message: "API base URL requires HTTPS" };
      return null;
    });

    render(<App />);
    await screen.findByTestId("setup-wizard");
    fireEvent.click(screen.getByText("Continue")); // workspace
    await screen.findByTestId("setup-step-workspace");
    fireEvent.click(screen.getByText("Continue"));
    await screen.findByTestId("setup-step-model");
    fireEvent.change(
      screen.getByPlaceholderText("gpt-5 / claude-sonnet-4-5 / …"),
      { target: { value: "gpt-5-mini" } }
    );
    fireEvent.change(screen.getByLabelText("API key"), {
      target: { value: "sk-x" },
    });
    fireEvent.click(screen.getByText("Continue"));
    await screen.findByTestId("setup-step-permissions");
    fireEvent.click(screen.getByText("Finish setup"));

    // The backend error is shown and the wizard stays open.
    await waitFor(() => {
      expect(screen.getByTestId("setup-wizard").textContent).toContain(
        "config_io: API base URL requires HTTPS"
      );
    });

    // Skipping dismisses the wizard but leaves the config warning visible.
    fireEvent.click(screen.getByText("Set up later"));
    await waitFor(() => {
      expect(screen.queryByTestId("setup-wizard")).toBeNull();
    });
    const t = getMessages("en-US");
    expect(document.body.textContent).toContain(t.home.configIncomplete);
  });

  it("opens the @ mention palette and inserts the picked reference", async () => {
    isTauriMock.mockReturnValue(true);
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_bootstrap_state") return sampleBootstrap();
      if (cmd === "list_sessions") return samplePage();
      if (cmd === "get_global_prompt")
        return { content: "", path: "/tmp/AGENTS.md", source_path: null };
      if (cmd === "list_extensions")
        return [
          {
            kind: "skill",
            id: "sk-1",
            name: "pdf-extract",
            description: "Extract text from PDFs",
            scope: "workspace",
            version: null,
            active: true,
          },
          {
            kind: "plugin",
            id: "pl-1",
            name: "git-tools",
            description: "Extra git commands",
            scope: "user",
            version: "1.0.0",
            active: true,
          },
          {
            kind: "rule",
            id: "rl-1",
            name: "hidden-rule",
            description: "not mentionable",
            scope: "workspace",
            version: null,
            active: true,
          },
        ];
      if (cmd === "list_mcp_servers")
        return [
          {
            name: "docs-search",
            transport: "stdio",
            enabled: true,
            target: "docs-mcp",
            scopes: [],
            credential_ref: null,
          },
        ];
      return null;
    });

    render(<App />);
    const input = await screen.findByLabelText("Prompt input");
    fireEvent.change(input, { target: { value: "hi @" } });

    const palette = await screen.findByTestId("mention-palette");
    await waitFor(() => {
      expect(palette.textContent).toContain("@pdf-extract");
    });
    expect(palette.textContent).toContain("@git-tools");
    expect(palette.textContent).toContain("@docs-search");
    expect(palette.textContent).not.toContain("hidden-rule");

    // Filtering by query narrows the list.
    fireEvent.change(input, { target: { value: "hi @pdf" } });
    await waitFor(() => {
      expect(palette.textContent).toContain("@pdf-extract");
    });
    expect(palette.textContent).not.toContain("@git-tools");

    fireEvent.mouseDown(screen.getByText("@pdf-extract"));
    await waitFor(() => {
      expect((input as HTMLTextAreaElement).value).toBe(
        "hi @pdf-extract "
      );
    });
    expect(screen.queryByTestId("mention-palette")).toBeNull();
  });

  it("stages clipboard files pasted into the composer", async () => {
    isTauriMock.mockReturnValue(true);
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_bootstrap_state") return sampleBootstrap();
      if (cmd === "list_sessions") return samplePage();
      if (cmd === "get_global_prompt")
        return { content: "", path: "/tmp/AGENTS.md", source_path: null };
      if (cmd === "stage_pasted_attachments")
        return [
          {
            id: "draft_paste_1",
            token: "att_tok_p1",
            path: "/tmp/soyo-paste-1/1-screenshot.png",
            filename: "screenshot.png",
            mime: "image/png",
            size_bytes: 32,
            kind: "image",
            preview_url: "soyo-attachment://localhost/draft/draft_paste_1",
          },
        ];
      return null;
    });

    render(<App />);
    const input = await screen.findByLabelText("Prompt input");
    const file = new File([new Uint8Array([1, 2, 3])], "screenshot.png", {
      type: "image/png",
    });
    const pasteEvent = new Event("paste", {
      bubbles: true,
      cancelable: true,
    });
    Object.defineProperty(pasteEvent, "clipboardData", {
      value: { files: [file], getData: () => "" },
    });
    fireEvent(input, pasteEvent);

    await waitFor(() => {
      expect(
        invokeMock.mock.calls.some(
          ([cmd]) => cmd === "stage_pasted_attachments"
        )
      ).toBe(true);
    });
    const [, args] = invokeMock.mock.calls.find(
      ([cmd]) => cmd === "stage_pasted_attachments"
    )!;
    expect((args as { items: { name: string }[] }).items[0].name).toBe(
      "screenshot.png"
    );
    expect((args as { items: { data: number[] }[] }).items[0].data).toEqual([
      1, 2, 3,
    ]);
    await waitFor(() => {
      expect(document.body.textContent).toContain("screenshot.png");
    });
  });

  it("shows permanent danger Bypass badges, SquaresLoader during active run, and pins sessionId on submit_prompt", async () => {
    const boot = {
      ...sampleBootstrap(),
      permission_mode: "bypass" as const,
    };
    const submittedArgs: Record<string, unknown>[] = [];
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "get_bootstrap_state") return boot;
      if (cmd === "list_sessions") return samplePage();
      if (cmd === "submit_prompt") {
        submittedArgs.push(args ?? {});
        return {
          action: "started",
          session_id: boot.session!.id,
          run_id: "run_bypass_1",
        };
      }
      return null;
    });

    const { container } = render(<App />);
    await waitFor(() => {
      expect(screen.getByTestId("bypass-mode-badge")).toBeTruthy();
      expect(screen.getByTestId("composer-bypass-badge")).toBeTruthy();
    });

    expect(screen.getByTestId("bypass-mode-badge").className).toContain("--danger");
    expect(screen.getByTestId("composer-bypass-badge").className).toContain("--danger");

    fireEvent.change(screen.getByLabelText("Prompt input"), {
      target: { value: "Verify session pinning" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => {
      expect(submittedArgs.length).toBe(1);
      expect(submittedArgs[0]).toMatchObject({
        prompt: "Verify session pinning",
        sessionId: boot.session!.id,
      });
      expect(container.querySelector(".soyo-squares-loader")).toBeTruthy();
    });
  });

  it("switches Light/Dark theme, persists desktop prefs, passes utcOffsetMinutes on usage stats, and handles shortcuts & 900px breakpoint", async () => {
    isTauriMock.mockReturnValue(true);
    const updatedPrefs: Record<string, unknown>[] = [];
    const usageCalls: Record<string, unknown>[] = [];
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "get_bootstrap_state") return sampleBootstrap();
      if (cmd === "list_sessions") return samplePage();
      if (cmd === "get_global_prompt")
        return { content: "", path: "/tmp/AGENTS.md", source_path: null };
      if (cmd === "get_helper_status") {
        return {
          connected: true,
          version: "0.3.0",
          pid: 1234,
          active_runs: 0,
          active_clients: 1,
          detail: null,
        };
      }
      if (cmd === "update_desktop_prefs") {
        updatedPrefs.push((args as { params: Record<string, unknown> }).params);
        return {
          nickname: null,
          avatar_data: null,
          theme: "light",
          font_size: 14,
          conversation_font: "default",
          reduce_motion: false,
          inspector_open: true,
        };
      }
      if (cmd === "get_usage_stats") {
        usageCalls.push(args ?? {});
        return {
          days_requested: 30,
          since: 1700000000,
          totals: { input_tokens: 0, output_tokens: 0, cached_input_tokens: 0, runs: 0, requests: 0, estimated: false },
          by_purpose: [],
          days: [],
        };
      }
      return null;
    });

    render(<App />);
    await waitFor(() => {
      expect(screen.getByTestId("context-sidebar")).toBeTruthy();
      expect(screen.getByTestId("sidebar-panel").className).toContain("w-[248px]");
    });

    // ⌘B toggles sidebar
    fireEvent.keyDown(window, { key: "b", metaKey: true });
    await waitFor(() => {
      expect(screen.getByTestId("sidebar-panel").className).toContain("w-[48px]");
    });

    // ⌘K opens Search overlay modal
    fireEvent.keyDown(window, { key: "k", metaKey: true });
    await waitFor(() => {
      expect(screen.getByTestId("search-overlay-modal")).toBeTruthy();
    });
    fireEvent.keyDown(window, { key: "k", metaKey: true });
    await waitFor(() => {
      expect(screen.queryByTestId("search-overlay-modal")).toBeNull();
    });

    // ⌘, opens SettingsDialog
    fireEvent.keyDown(window, { key: ",", metaKey: true });
    await waitFor(() => {
      expect(screen.getByTestId("settings-dialog")).toBeTruthy();
    });

    // Switch to Appearance -> Light updates data-theme & persists via update_desktop_prefs
    fireEvent.click(screen.getByRole("button", { name: "Appearance" }));
    fireEvent.click(screen.getByRole("button", { name: "Light" }));
    await waitFor(() => {
      expect(document.documentElement.dataset.theme).toBe("light");
      expect(updatedPrefs.some((p) => p.theme === "light")).toBe(true);
    });

    // Usage tab sends numeric utcOffsetMinutes (D-078)
    fireEvent.click(screen.getByRole("button", { name: "Usage" }));
    await waitFor(() => {
      expect(usageCalls.length).toBeGreaterThan(0);
      expect(typeof usageCalls[0].utcOffsetMinutes).toBe("number");
    });
    fireEvent.click(screen.getByLabelText("Close settings"));

    // Narrow 900px viewport collapses sidebar to 48px rail and hides docked inspector
    act(() => {
      Object.defineProperty(window, "innerWidth", {
        configurable: true,
        writable: true,
        value: 900,
      });
      window.dispatchEvent(new Event("resize"));
    });
    await waitFor(() => {
      expect(screen.getByTestId("sidebar-panel").className).toContain("w-[48px]");
      expect(screen.queryByTestId("context-sidebar")).toBeNull();
    });
  });
});
