import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  BootstrapStateDto_Serialize,
  DesktopStreamEnvelope_Deserialize,
  SessionPageDto_Serialize,
} from "./bindings";

const { invokeMock, MockChannel, checkUpdateMock } = vi.hoisted(() => {
  class HoistedChannel<T> {
    id = Math.floor(Math.random() * 100000);
    onmessage: (message: T) => void = () => {};
  }
  return {
    invokeMock: vi.fn(),
    MockChannel: HoistedChannel,
    checkUpdateMock: vi.fn(),
  };
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
  Channel: MockChannel,
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
import { useAppStore } from "./store/useAppStore";

function sampleBootstrap(): BootstrapStateDto_Serialize {
  return {
    version: "0.2.0",
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
    useAppStore.setState({
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
      configChangeSessionId: null,
      recoveryBanner: null,
      error: null,
    });
  });

  it("hydrates bootstrap state, streams prompt events via Channel, and cancels cleanly", async () => {
    let capturedChannel: {
      onmessage: (message: DesktopStreamEnvelope_Deserialize) => void;
    } | null = null;

    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "get_bootstrap_state") {
        return sampleBootstrap();
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

    const cancelBtn = screen.getByRole("button", { name: "Cancel" });
    fireEvent.click(cancelBtn);

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("cancel_run");
      expect(
        screen.getAllByText("claude-sonnet-4-5 · cancelled").length
      ).toBeGreaterThan(0);
    });

    // Global Markdown Source / Render toggle switches all assistant markdown cards.
    const sourceToggle = screen.getByRole("button", { name: "Source" });
    fireEvent.click(sourceToggle);
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
      expect(screen.getByTestId("mode-permission-bar")).toBeTruthy();
    });

    // Empty composer Tab cycles WORK -> CODE
    const input = screen.getByLabelText("Prompt input");
    fireEvent.keyDown(input, { key: "Tab" });
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_work_mode", { mode: "code" });
    });

    // Switch permission mode to manual
    const permSelect = screen.getByLabelText("Permission mode");
    fireEvent.change(permSelect, { target: { value: "manual" } });
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
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    await waitFor(() => {
      expect(screen.getByTestId("settings-dialog")).toBeTruthy();
      expect(screen.getByTestId("modules-panel")).toBeTruthy();
    });

    const wsInput = screen.getByLabelText("Workspace Directory");
    fireEvent.change(wsInput, { target: { value: "/workspace/updated" } });
    fireEvent.click(screen.getByRole("button", { name: "Apply" }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_workspace", {
        path: "/workspace/updated",
      });
    });

    fireEvent.click(screen.getByRole("button", { name: "Enabled" }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_module_enabled", {
        module: "mcp",
        enabled: false,
      });
    });
    fireEvent.click(screen.getByLabelText("Close settings"));

    // MCP & Credentials
    fireEvent.click(screen.getByRole("button", { name: "MCP & Creds" }));
    await waitFor(() => {
      expect(screen.getByTestId("mcp-credentials-dialog")).toBeTruthy();
      expect(screen.getByText(/docs-http \(bearer_env\)/)).toBeTruthy();
    });

    fireEvent.change(screen.getByLabelText("Secret environment variable"), {
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
    fireEvent.click(screen.getByLabelText("Close MCP and credentials dialog"));

    // Memory two-phase review/confirm
    fireEvent.click(screen.getByRole("button", { name: "Memory" }));
    await waitFor(() => {
      expect(screen.getByTestId("memory-dialog")).toBeTruthy();
      expect(invokeMock).toHaveBeenCalledWith("query_memory", { command: "list" });
    });

    fireEvent.change(screen.getByLabelText("Prepare memory command"), {
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
    fireEvent.click(screen.getByRole("button", { name: "Export" }));
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

    // Stage attachment & submit
    fireEvent.click(screen.getByRole("button", { name: "Attach" }));
    fireEvent.change(screen.getByLabelText("Attachment file path"), {
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

      fireEvent.click(screen.getByLabelText("Dismiss update banner"));
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
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    await waitFor(() => {
      expect(screen.getByTestId("settings-dialog")).toBeTruthy();
    });
    fireEvent.click(screen.getByRole("button", { name: "Usage" }));
    await waitFor(() => {
      expect(screen.getByTestId("usage-panel")).toBeTruthy();
      expect(screen.getByText("Tokens per day")).toBeTruthy();
    });
  });
});
