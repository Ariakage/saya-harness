import { Channel } from "@tauri-apps/api/core";
import { create } from "zustand";
import {
  commands,
  type ActiveSessionDto_Serialize,
  type ApprovalRequestDto_Deserialize,
  type AttachmentTokenDto,
  type BootstrapStateDto_Serialize,
  type ContextEntryDto,
  type DesktopError,
  type DesktopStreamEnvelope_Deserialize,
  type ErrorInfoDto,
  type InputRequestDto,
  type McpStatusBadgeDto,
  type MessageRoleDto,
  type PermissionModeDto,
  type RestoredMessageDto_Serialize,
  type SchemaEnvelopeDto_Deserialize,
  type SessionItemDto_Serialize,
  type StagedAttachmentDto,
  type ThinkingSnapshotDto,
  type TodoItemDto,
  type UsageDto_Serialize,
  type WorkModeDto,
} from "../bindings";
import {
  DEFAULT_LOCALE,
  type LocaleId,
} from "../lib/i18n";
import { sanitizeDisplayText } from "../lib/sanitize";

export type MarkdownDisplayMode = "render" | "source";

export interface DesktopMessage {
  id: string;
  role: MessageRoleDto;
  label: string;
  text: string;
  streaming: boolean;
  truncated: boolean;
  toolCallId?: string | null;
  toolName?: string | null;
  toolSummary?: string | null;
  toolOk?: boolean | null;
  toolState?: "requested" | "running" | "ok" | "error";
  toolRequestFormatted?: string;
  toolReviewNote?: string;
  toolError?: ErrorInfoDto | null;
  mediaRefs: string[];
}

export interface AppState {
  initialized: boolean;
  loading: boolean;
  bootstrap: BootstrapStateDto_Serialize | null;
  sessionId: string | null;
  sessionTitle: string;
  historyOmitted: boolean;
  workMode: WorkModeDto;
  pendingMode: WorkModeDto | null;
  permissionMode: PermissionModeDto;
  messages: DesktopMessage[];
  thinkingEntries: ThinkingSnapshotDto[];
  contextEntries: ContextEntryDto[];
  contextSkillsCount: number | null;
  contextEstimated: boolean;
  mcpServers: McpStatusBadgeDto[];
  todos: TodoItemDto[];
  lastUsage: UsageDto_Serialize | null;
  recentSessions: SessionItemDto_Serialize[];
  runActive: boolean;
  cancelling: boolean;
  activeRunId: string | null;
  summaryLine: string | null;
  markdownMode: MarkdownDisplayMode;
  locale: LocaleId;
  pendingApproval: ApprovalRequestDto_Deserialize | null;
  pendingInput: InputRequestDto | null;
  stagedAttachments: StagedAttachmentDto[];
  ultracodeEnabled: boolean;
  submitMode: "steer" | "queue";
  configChangeSessionId: string | null;
  recoveryBanner: string | null;
  userName: string | null;
  error: DesktopError | null;

  initApp: () => Promise<void>;
  applyBootstrap: (boot: BootstrapStateDto_Serialize) => void;
  setThinkingEffort: (effort: string | null, ultracode?: boolean) => Promise<boolean>;
  selectProfile: (profile: string) => Promise<boolean>;
  setSubmitMode: (mode: "steer" | "queue") => void;
  refreshSessions: (page?: number) => Promise<void>;
  startNewSession: () => Promise<void>;
  stageAttachments: (paths: string[]) => Promise<boolean>;
  removeStagedAttachment: (id: string) => void;
  submitPrompt: (
    prompt: string,
    attachments?: AttachmentTokenDto[]
  ) => Promise<boolean>;
  cancelActiveRun: () => Promise<void>;
  compactCurrentSession: () => Promise<boolean>;
  replyApproval: (approved: boolean) => Promise<boolean>;
  replyInput: (answer: string) => Promise<boolean>;
  setWorkMode: (mode: WorkModeDto) => Promise<boolean>;
  cycleWorkMode: () => Promise<boolean>;
  setPermissionMode: (permissions: PermissionModeDto) => Promise<boolean>;
  resumeSession: (
    sessionId: string,
    acceptConfigChange?: boolean
  ) => Promise<"ok" | "needs_confirm" | "error">;
  clearConfigChangePrompt: () => void;
  setMarkdownMode: (mode: MarkdownDisplayMode) => void;
  toggleMarkdownMode: () => void;
  setLocale: (locale: LocaleId) => void;
  clearError: () => void;
  handleStreamEnvelope: (envelope: DesktopStreamEnvelope_Deserialize) => void;
}

const MAX_RENDERED_MESSAGES = 400;

function trimMessages(messages: DesktopMessage[]): DesktopMessage[] {
  if (messages.length <= MAX_RENDERED_MESSAGES) {
    return messages;
  }
  return messages.slice(messages.length - MAX_RENDERED_MESSAGES);
}

function fromRestoredMessage(msg: RestoredMessageDto_Serialize): DesktopMessage {
  return {
    id: msg.id,
    role: msg.role,
    label: sanitizeDisplayText(msg.label),
    text: sanitizeDisplayText(msg.text),
    streaming: msg.streaming,
    truncated: msg.truncated,
    toolCallId: msg.tool_call_id,
    toolName: msg.tool_name,
    toolSummary: msg.tool_summary ? sanitizeDisplayText(msg.tool_summary) : null,
    toolOk: msg.tool_ok,
    toolState:
      msg.tool_ok === true ? "ok" : msg.tool_ok === false ? "error" : undefined,
    mediaRefs: msg.media_refs,
  };
}

function applySessionDto(
  session: ActiveSessionDto_Serialize | null
): Partial<AppState> {
  if (!session) {
    return {
      sessionId: null,
      sessionTitle: "New session",
      historyOmitted: false,
      messages: [],
      todos: [],
      lastUsage: null,
    };
  }
  return {
    sessionId: session.id,
    sessionTitle: sanitizeDisplayText(session.title || "Untitled session"),
    workMode: session.work_mode,
    historyOmitted: session.history_omitted,
    messages: trimMessages(session.messages.map(fromRestoredMessage)),
    todos: session.todos,
    lastUsage: session.last_usage,
  };
}

function normalizeError(err: unknown): DesktopError {
  if (
    typeof err === "object" &&
    err !== null &&
    "code" in err &&
    "message" in err
  ) {
    const e = err as { code: unknown; message: unknown };
    return {
      code: String(e.code),
      message: sanitizeDisplayText(String(e.message)),
    };
  }
  return {
    code: "ipc_error",
    message: sanitizeDisplayText(err instanceof Error ? err.message : String(err)),
  };
}

function applySchemaEvent(
  state: AppState,
  envelope: SchemaEnvelopeDto_Deserialize
): Partial<AppState> {
  const payload = envelope.payload;
  const runPrefix = envelope.run_id ?? envelope.session_id;

  switch (payload.type) {
    case "session.created":
    case "session.resumed":
      return {
        sessionId: envelope.session_id,
        workMode: payload.data.work_mode,
        permissionMode: payload.data.permissions,
      };
    case "run.started":
      return {
        runActive: true,
        activeRunId: envelope.run_id ?? state.activeRunId,
        workMode: payload.data.work_mode,
        permissionMode: payload.data.permissions,
        summaryLine: null,
      };
    case "mode.change_requested":
      return {
        pendingMode: payload.data.to,
      };
    case "mode.changed":
      return {
        workMode: payload.data.to,
        pendingMode: null,
      };
    case "model.started":
      return {};
    case "model.text_delta": {
      const msgId = `model:${runPrefix}:${payload.data.request_id}`;
      const chunk = sanitizeDisplayText(payload.data.text);
      const idx = state.messages.findIndex((m) => m.id === msgId);
      if (idx >= 0) {
        const next = [...state.messages];
        next[idx] = {
          ...next[idx],
          text: next[idx].text + chunk,
          streaming: true,
        };
        return { messages: next };
      }
      return {
        messages: trimMessages([
          ...state.messages,
          {
            id: msgId,
            role: "assistant",
            label: "Assistant",
            text: chunk,
            streaming: true,
            truncated: false,
            mediaRefs: [],
          },
        ]),
      };
    }
    case "model.finished": {
      const msgId = `model:${runPrefix}:${payload.data.request_id}`;
      const next = state.messages.map((m) =>
        m.id === msgId ? { ...m, streaming: false } : m
      );
      return {
        messages: next,
        lastUsage: payload.data.usage
          ? {
              input_tokens: payload.data.usage.input_tokens ?? null,
              output_tokens: payload.data.usage.output_tokens ?? null,
              cached_input_tokens: payload.data.usage.cached_input_tokens ?? null,
              estimated: payload.data.usage.estimated,
            }
          : state.lastUsage,
        error:
          !payload.data.ok && payload.data.error
            ? {
                code: payload.data.error.code,
                message: sanitizeDisplayText(payload.data.error.message),
              }
            : state.error,
      };
    }
    case "tool.requested": {
      const msgId = `tool:${payload.data.call_id}`;
      const existing = state.messages.findIndex((m) => m.id === msgId);
      const formatted = sanitizeDisplayText(payload.data.formatted);
      const summary = payload.data.summary
        ? sanitizeDisplayText(payload.data.summary)
        : null;
      const entry: DesktopMessage = {
        id: msgId,
        role: "tool",
        label: `Tool · ${payload.data.name}`,
        text: formatted,
        toolRequestFormatted: formatted,
        streaming: true,
        truncated: false,
        toolCallId: payload.data.call_id,
        toolName: payload.data.name,
        toolSummary: summary,
        toolOk: null,
        toolState: "requested",
        mediaRefs: [],
      };
      if (existing >= 0) {
        const next = [...state.messages];
        next[existing] = { ...next[existing], ...entry };
        return { messages: next };
      }
      return {
        messages: trimMessages([...state.messages, entry]),
      };
    }
    case "permission.reviewed": {
      const msgId = `tool:${payload.data.call_id}`;
      const note = sanitizeDisplayText(
        `${payload.data.source}: ${payload.data.decision} (${payload.data.reason})`
      );
      return {
        messages: state.messages.map((m) =>
          m.id === msgId ? { ...m, toolReviewNote: note } : m
        ),
      };
    }
    case "tool.started": {
      const msgId = `tool:${payload.data.call_id}`;
      return {
        messages: state.messages.map((m) =>
          m.id === msgId ? { ...m, toolState: "running", streaming: true } : m
        ),
      };
    }
    case "tool.finished": {
      const msgId = `tool:${payload.data.call_id}`;
      const res = payload.data.result;
      const formatted = sanitizeDisplayText(res.formatted);
      const summary = res.summary ? sanitizeDisplayText(res.summary) : null;
      const idx = state.messages.findIndex((m) => m.id === msgId);
      if (idx >= 0) {
        const next = [...state.messages];
        next[idx] = {
          ...next[idx],
          text: formatted,
          streaming: false,
          truncated: res.truncated,
          toolSummary: summary ?? next[idx].toolSummary,
          toolOk: res.ok,
          toolState: res.ok ? "ok" : "error",
          toolError: res.error ?? null,
          mediaRefs: res.media_refs,
        };
        return { messages: next };
      }
      return {
        messages: trimMessages([
          ...state.messages,
          {
            id: msgId,
            role: "tool",
            label: `Tool · ${payload.data.name}`,
            text: formatted,
            streaming: false,
            truncated: res.truncated,
            toolCallId: payload.data.call_id,
            toolName: payload.data.name,
            toolSummary: summary,
            toolOk: res.ok,
            toolState: res.ok ? "ok" : "error",
            toolError: res.error ?? null,
            mediaRefs: res.media_refs,
          },
        ]),
      };
    }
    case "mcp.status":
      return { mcpServers: payload.data.servers };
    case "run.finished":
      return {
        lastUsage: payload.data.usage
          ? {
              input_tokens: payload.data.usage.input_tokens ?? null,
              output_tokens: payload.data.usage.output_tokens ?? null,
              cached_input_tokens: payload.data.usage.cached_input_tokens ?? null,
              estimated: payload.data.usage.estimated,
            }
          : state.lastUsage,
      };
    default:
      return {};
  }
}

export const useAppStore = create<AppState>((set, get) => {
  const createEventChannel = () => {
    const channel = new Channel<DesktopStreamEnvelope_Deserialize>();
    channel.onmessage = (envelope) => {
      get().handleStreamEnvelope(envelope);
    };
    return channel;
  };

  return {
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
    locale: DEFAULT_LOCALE,
    pendingApproval: null,
    pendingInput: null,
    stagedAttachments: [],
    ultracodeEnabled: false,
    submitMode: "steer",
    configChangeSessionId: null,
    recoveryBanner: null,
    userName: null,
    error: null,

    applyBootstrap: (boot) =>
      set((state) => ({
        bootstrap: boot,
        workMode: boot.work_mode,
        pendingMode: boot.pending_mode,
        permissionMode: boot.permission_mode,
        runActive: boot.run_active,
        activeRunId: boot.active_run_id,
        error: null,
        ...(boot.session && boot.session.id !== state.sessionId
          ? applySessionDto(boot.session)
          : boot.session === null
            ? applySessionDto(null)
            : {}),
      })),

    setThinkingEffort: async (effort, ultracode = false) => {
      try {
        const res = await commands.setThinkingEffort(effort);
        if (res.status === "ok") {
          set({ ultracodeEnabled: ultracode });
          get().applyBootstrap(res.data);
          return true;
        }
        set({ error: res.error });
        return false;
      } catch (err) {
        set({ error: normalizeError(err) });
        return false;
      }
    },

    selectProfile: async (profile) => {
      try {
        const res = await commands.selectProfile(profile);
        if (res.status === "ok") {
          get().applyBootstrap(res.data);
          return true;
        }
        set({ error: res.error });
        return false;
      } catch (err) {
        set({ error: normalizeError(err) });
        return false;
      }
    },

    setSubmitMode: (mode) => set({ submitMode: mode }),

    initApp: async () => {
      set({ loading: true });
      try {
        const [bootRes, listRes, nameRes] = await Promise.all([
          commands.getBootstrapState(),
          commands.listSessions(1),
          commands.getUserName(),
        ]);
        const updates: Partial<AppState> = {
          initialized: true,
          loading: false,
          userName:
            nameRes.status === "ok" && nameRes.data?.trim()
              ? nameRes.data.trim()
              : null,
        };
        if (bootRes.status === "ok") {
          updates.bootstrap = bootRes.data;
          updates.workMode = bootRes.data.work_mode;
          updates.pendingMode = bootRes.data.pending_mode;
          updates.permissionMode = bootRes.data.permission_mode;
          updates.runActive = bootRes.data.run_active;
          updates.activeRunId = bootRes.data.active_run_id;
          Object.assign(updates, applySessionDto(bootRes.data.session));
        } else {
          updates.error = bootRes.error;
        }
        if (listRes.status === "ok") {
          updates.recentSessions = listRes.data.items;
        }
        set(updates);
      } catch (err) {
        set({
          initialized: true,
          loading: false,
          error: normalizeError(err),
        });
      }
    },

    refreshSessions: async (page = 1) => {
      try {
        const res = await commands.listSessions(page);
        if (res.status === "ok") {
          set({ recentSessions: res.data.items });
        }
      } catch (err) {
        set({ error: normalizeError(err) });
      }
    },

    startNewSession: async () => {
      try {
        const res = await commands.newSession();
        if (res.status === "ok") {
          set({
            ...applySessionDto(res.data),
            thinkingEntries: [],
            contextEntries: [],
            contextSkillsCount: null,
            summaryLine: null,
            pendingApproval: null,
            pendingInput: null,
            stagedAttachments: [],
            configChangeSessionId: null,
            recoveryBanner: null,
            error: null,
          });
          await get().refreshSessions(1);
        } else {
          set({ error: res.error });
        }
      } catch (err) {
        set({ error: normalizeError(err) });
      }
    },

    stageAttachments: async (paths) => {
      const cleaned = paths.map((p) => p.trim()).filter(Boolean);
      if (cleaned.length === 0) {
        return false;
      }
      try {
        const res = await commands.stageAttachments(cleaned);
        if (res.status !== "ok") {
          set({ error: res.error });
          return false;
        }
        set((state) => {
          const existingIds = new Set(state.stagedAttachments.map((a) => a.id));
          const merged = [
            ...state.stagedAttachments,
            ...res.data.filter((item) => !existingIds.has(item.id)),
          ];
          return {
            stagedAttachments: merged,
            error: null,
          };
        });
        return true;
      } catch (err) {
        set({ error: normalizeError(err) });
        return false;
      }
    },

    removeStagedAttachment: (id) =>
      set((state) => ({
        stagedAttachments: state.stagedAttachments.filter((a) => a.id !== id),
      })),

    submitPrompt: async (prompt, attachments = []) => {
      const trimmed = prompt.trim();
      const staged = get().stagedAttachments.map((a) => ({
        token: a.token,
        path: a.path,
      }));
      const mergedAttachments = [
        ...attachments,
        ...staged.filter((s) => !attachments.some((a) => a.token === s.token)),
      ];
      if (!trimmed && mergedAttachments.length === 0) {
        return false;
      }
      const channel = createEventChannel();
      try {
        const res = await commands.submitPrompt(trimmed, mergedAttachments, channel);
        if (res.status !== "ok") {
          set({ error: res.error });
          return false;
        }
        const userMessage: DesktopMessage = {
          id: `user:${res.data.run_id}:${Date.now()}`,
          role: "user",
          label: res.data.action === "steered" ? "Steer" : "You",
          text: sanitizeDisplayText(trimmed),
          streaming: false,
          truncated: false,
          mediaRefs: [],
        };
        set((state) => ({
          sessionId: res.data.session_id || state.sessionId,
          activeRunId: res.data.run_id,
          runActive: true,
          cancelling: false,
          stagedAttachments: [],
          error: null,
          summaryLine:
            res.data.action === "steered" ? state.summaryLine : null,
          messages: trimMessages([...state.messages, userMessage]),
        }));
        return true;
      } catch (err) {
        set({ error: normalizeError(err) });
        return false;
      }
    },

    cancelActiveRun: async () => {
      set({ cancelling: true });
      try {
        const res = await commands.cancelRun();
        if (res.status === "ok") {
          if (!res.data) {
            set({ cancelling: false, runActive: false });
          }
        } else {
          set({ cancelling: false, error: res.error });
        }
      } catch (err) {
        set({ cancelling: false, error: normalizeError(err) });
      }
    },

    compactCurrentSession: async () => {
      const channel = createEventChannel();
      try {
        const res = await commands.compactSession(channel);
        if (res.status !== "ok") {
          set({ error: res.error });
          return false;
        }
        set({
          activeRunId: res.data.run_id,
          runActive: true,
          error: null,
        });
        return true;
      } catch (err) {
        set({ error: normalizeError(err) });
        return false;
      }
    },

    replyApproval: async (approved) => {
      const pending = get().pendingApproval;
      if (!pending) {
        return false;
      }
      try {
        const res = await commands.replyApproval(
          pending.approval_id,
          pending.call_id,
          approved
        );
        set({ pendingApproval: null });
        if (res.status !== "ok") {
          set({ error: res.error });
          return false;
        }
        return true;
      } catch (err) {
        set({ pendingApproval: null, error: normalizeError(err) });
        return false;
      }
    },

    replyInput: async (answer) => {
      const pending = get().pendingInput;
      if (!pending) {
        return false;
      }
      const trimmed = answer.trim();
      if (!trimmed) {
        return false;
      }
      try {
        const res = await commands.replyInput(pending.input_id, trimmed);
        set({ pendingInput: null });
        if (res.status !== "ok") {
          set({ error: res.error });
          return false;
        }
        return true;
      } catch (err) {
        set({ pendingInput: null, error: normalizeError(err) });
        return false;
      }
    },

    setWorkMode: async (mode) => {
      try {
        const res = await commands.setWorkMode(mode);
        if (res.status !== "ok") {
          set({ error: res.error });
          return false;
        }
        set((state) => ({
          workMode: res.data.current,
          pendingMode: res.data.pending,
          pendingApproval: res.data.active_run ? null : state.pendingApproval,
          error: null,
        }));
        return true;
      } catch (err) {
        set({ error: normalizeError(err) });
        return false;
      }
    },

    cycleWorkMode: async () => {
      const order: WorkModeDto[] = ["work", "code", "plan", "build"];
      const current = get().pendingMode ?? get().workMode;
      const next = order[(order.indexOf(current) + 1) % order.length];
      return get().setWorkMode(next);
    },

    setPermissionMode: async (permissions) => {
      try {
        const res = await commands.setPermissionMode(permissions);
        if (res.status !== "ok") {
          set({ error: res.error });
          return false;
        }
        set({ permissionMode: res.data, error: null });
        return true;
      } catch (err) {
        set({ error: normalizeError(err) });
        return false;
      }
    },

    resumeSession: async (sessionId, acceptConfigChange = false) => {
      try {
        const res = await commands.resumeSession(sessionId, acceptConfigChange);
        if (res.status !== "ok") {
          if (res.error.code === "config_change_requires_confirmation") {
            set({ configChangeSessionId: sessionId });
            return "needs_confirm";
          }
          set({ error: res.error, configChangeSessionId: null });
          return "error";
        }
        const boot = res.data.bootstrap;
        const recoveryParts: string[] = [];
        if (res.data.recovered_runs > 0) {
          recoveryParts.push(`${res.data.recovered_runs} interrupted run(s)`);
        }
        if (res.data.recovered_tools > 0) {
          recoveryParts.push(`${res.data.recovered_tools} closed tool call(s)`);
        }
        set({
          bootstrap: boot,
          workMode: boot.work_mode,
          pendingMode: boot.pending_mode,
          permissionMode: boot.permission_mode,
          runActive: boot.run_active,
          activeRunId: boot.active_run_id,
          ...applySessionDto(boot.session),
          thinkingEntries: [],
          contextEntries: [],
          summaryLine: null,
          pendingApproval: null,
          pendingInput: null,
          configChangeSessionId: null,
          recoveryBanner:
            recoveryParts.length > 0
              ? `Recovered ${recoveryParts.join(", ")}.`
              : null,
          error: null,
        });
        await get().refreshSessions(1);
        return "ok";
      } catch (err) {
        set({ error: normalizeError(err), configChangeSessionId: null });
        return "error";
      }
    },

    clearConfigChangePrompt: () => set({ configChangeSessionId: null }),

    setMarkdownMode: (mode) => set({ markdownMode: mode }),

    toggleMarkdownMode: () =>
      set((state) => ({
        markdownMode: state.markdownMode === "render" ? "source" : "render",
      })),

    setLocale: (locale) => {
      if (typeof document !== "undefined") {
        document.documentElement.lang = locale;
      }
      set({ locale });
    },

    clearError: () => set({ error: null }),

    handleStreamEnvelope: (envelope) => {
      set((state) => {
        switch (envelope.kind) {
          case "schema_event":
            return applySchemaEvent(state, envelope.data);
          case "thinking":
            return {
              thinkingEntries: envelope.data.map((entry) => ({
                ...entry,
                text: sanitizeDisplayText(entry.text),
              })),
            };
          case "context":
            return {
              contextEntries: envelope.data.entries,
              contextSkillsCount: envelope.data.skills_count ?? null,
              contextEstimated: envelope.data.estimated,
            };
          case "mode_state":
            return {
              workMode: envelope.data.current,
              pendingMode: envelope.data.pending ?? null,
            };
          case "approval_needed":
            return {
              pendingApproval: envelope.data,
            };
          case "input_needed":
            return {
              pendingInput: envelope.data,
            };
          case "run_completed": {
            const summaryText = sanitizeDisplayText(envelope.data.summary_line);
            const settled = state.messages.map((m) =>
              m.streaming ? { ...m, streaming: false } : m
            );
            const withSummary = trimMessages([
              ...settled,
              {
                id: `summary:${state.activeRunId ?? Date.now()}`,
                role: "run_summary",
                label: envelope.data.status.toUpperCase(),
                text: summaryText,
                streaming: false,
                truncated: false,
                mediaRefs: [],
              },
            ]);
            return {
              messages: withSummary,
              todos: envelope.data.todos,
              summaryLine: summaryText,
              runActive: false,
              cancelling: false,
              pendingApproval: null,
              pendingInput: null,
              error: envelope.data.error ?? state.error,
            };
          }
          default:
            return {};
        }
      });

      if (envelope.kind === "run_completed") {
        void get().refreshSessions(1);
      }
    },
  };
});
