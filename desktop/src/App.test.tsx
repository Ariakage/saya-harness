import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  BootstrapStateDto_Serialize,
  DesktopStreamEnvelope_Deserialize,
  SessionPageDto_Serialize,
} from "./bindings";

const { invokeMock, MockChannel } = vi.hoisted(() => {
  class HoistedChannel<T> {
    id = Math.floor(Math.random() * 100000);
    onmessage: (message: T) => void = () => {};
  }
  return {
    invokeMock: vi.fn(),
    MockChannel: HoistedChannel,
  };
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
  Channel: MockChannel,
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
});
