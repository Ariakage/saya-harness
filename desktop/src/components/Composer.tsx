import {
  ArrowUp,
  Check,
  ChevronDown,
  ChevronRight,
  Cpu,
  FileText,
  Globe,
  Layers,
  List,
  Minimize2,
  Paperclip,
  Plus,
  Puzzle,
  SlidersHorizontal,
  Square,
} from "lucide-react";
import { type KeyboardEvent, useEffect, useRef, useState } from "react";
import type {
  PermissionModeDto,
  ProfileOptionDto_Serialize,
  StagedAttachmentDto,
  WorkModeDto,
} from "../bindings";
import {
  PERMISSION_MODE_LABELS,
  WORK_MODE_STYLES,
  sanitizeDisplayText,
} from "../lib/sanitize";
import { StagedAttachmentsStrip } from "./AttachmentPreview";
import { EffortPill, EffortPopover } from "./EffortPopover";
import { MarkdownRenderer } from "./MarkdownRenderer";

export interface ComposerProps {
  disabled: boolean;
  runActive: boolean;
  cancelling: boolean;
  hasSession: boolean;
  approvalPending?: boolean;
  variant?: "docked" | "home";
  workMode?: WorkModeDto;
  permissionMode?: PermissionModeDto;
  selectedProfile?: string | null;
  selectedModel?: string | null;
  profiles?: ProfileOptionDto_Serialize[];
  thinkingEffort?: string | null;
  ultracodeEnabled?: boolean;
  submitMode?: "steer" | "queue";
  stagedAttachments?: StagedAttachmentDto[];
  forcePopover?: "attach" | "commands" | "effort" | "model" | null;
  draftSeed?: { text: string; nonce: number } | null;
  onSubmit: (prompt: string) => Promise<boolean>;
  onCancel: () => Promise<void>;
  onCompact: () => Promise<boolean>;
  onCycleMode?: () => Promise<boolean>;
  onSelectProfile?: (profile: string) => Promise<boolean>;
  onSelectEffort?: (effort: string, ultracode: boolean) => Promise<boolean>;
  onToggleSubmitMode?: () => void;
  onStageAttachments?: (paths: string[]) => Promise<boolean>;
  onRemoveAttachment?: (id: string) => void;
  onOpenSettings?: () => void;
  onOpenSessions?: () => void;
  onOpenMemory?: () => void;
  onOpenMcp?: () => void;
  onOpenExport?: () => void;
}

interface SlashCommandItem {
  cmd: string;
  desc: string;
  action: () => void;
}

export function Composer({
  disabled,
  runActive,
  cancelling,
  hasSession,
  approvalPending = false,
  variant = "docked",
  workMode = "work",
  permissionMode = "auto",
  selectedProfile = null,
  selectedModel = null,
  profiles = [],
  thinkingEffort = "medium",
  ultracodeEnabled = false,
  submitMode = "steer",
  stagedAttachments = [],
  forcePopover = null,
  draftSeed = null,
  onSubmit,
  onCancel,
  onCompact,
  onCycleMode,
  onSelectProfile,
  onSelectEffort,
  onToggleSubmitMode,
  onStageAttachments,
  onRemoveAttachment,
  onOpenSettings,
  onOpenSessions,
  onOpenMemory,
  onOpenMcp,
  onOpenExport,
}: ComposerProps) {
  const [draft, setDraft] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const [sending, setSending] = useState(false);
  const [attachInputOpen, setAttachInputOpen] = useState(false);
  const [attachPath, setAttachPath] = useState("");
  const [staging, setStaging] = useState(false);
  const [webSearchActive, setWebSearchActive] = useState(false);
  const [activePopover, setActivePopover] = useState<
    "attach" | "commands" | "effort" | "model" | null
  >(forcePopover);

  useEffect(() => {
    setActivePopover(forcePopover);
  }, [forcePopover]);

  useEffect(() => {
    if (!draftSeed) return;
    setDraft(draftSeed.text);
    const el = textareaRef.current;
    if (el) {
      el.focus();
      el.setSelectionRange(el.value.length, el.value.length);
    }
  }, [draftSeed]);

  const trimmed = draft.trim();
  const hasInput = trimmed.length > 0;
  const isStopState = runActive && !hasInput;
  const canSend =
    !disabled &&
    !sending &&
    !approvalPending &&
    (hasInput || stagedAttachments.length > 0);

  const slashOpen =
    activePopover === "commands" ||
    (draft.startsWith("/") && !draft.includes("\n") && !draft.includes(" "));

  const slashCommands: SlashCommandItem[] = [
    {
      cmd: "/model",
      desc: "switch model profile",
      action: () => {
        setDraft("");
        setActivePopover("model");
      },
    },
    {
      cmd: "/mode",
      desc: "change work mode — plan · build · code · work",
      action: () => {
        setDraft("");
        setActivePopover(null);
        void onCycleMode?.();
      },
    },
    {
      cmd: "/permission",
      desc: "auto · bypass permissions · manual approval",
      action: () => {
        setDraft("");
        setActivePopover(null);
        onOpenSettings?.();
      },
    },
    {
      cmd: "/compact",
      desc: "summarize context to free window space",
      action: () => {
        setDraft("");
        setActivePopover(null);
        void onCompact();
      },
    },
    {
      cmd: "/export",
      desc: "export session to markdown handoff",
      action: () => {
        setDraft("");
        setActivePopover(null);
        onOpenExport?.();
      },
    },
    {
      cmd: "/sessions",
      desc: "browse and restore sessions",
      action: () => {
        setDraft("");
        setActivePopover(null);
        onOpenSessions?.();
      },
    },
    {
      cmd: "/memory",
      desc: "view and edit workspace memory",
      action: () => {
        setDraft("");
        setActivePopover(null);
        onOpenMemory?.();
      },
    },
    {
      cmd: "/mcp",
      desc: "manage MCP servers and credentials",
      action: () => {
        setDraft("");
        setActivePopover(null);
        onOpenMcp?.();
      },
    },
  ];

  const handleStage = async () => {
    const path = attachPath.trim();
    if (!path || !onStageAttachments || staging || disabled) {
      return;
    }
    setStaging(true);
    try {
      const ok = await onStageAttachments([path]);
      if (ok) {
        setAttachPath("");
        setAttachInputOpen(false);
      }
    } finally {
      setStaging(false);
    }
  };

  const handleActionClick = async () => {
    if (isStopState) {
      if (!cancelling) {
        await onCancel();
      }
      return;
    }
    if (!canSend) {
      return;
    }
    setSending(true);
    try {
      const ok = await onSubmit(trimmed);
      if (ok) {
        setDraft("");
        setActivePopover(null);
      }
    } finally {
      setSending(false);
    }
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Tab" && draft.length === 0 && onCycleMode) {
      event.preventDefault();
      void onCycleMode();
      return;
    }
    if (event.key === "Escape") {
      if (activePopover) {
        event.preventDefault();
        setActivePopover(null);
        return;
      }
      if (runActive) {
        event.preventDefault();
        void onCancel();
      }
      return;
    }
    if (
      event.key === "Enter" &&
      !event.shiftKey &&
      !event.altKey &&
      !event.nativeEvent.isComposing
    ) {
      event.preventDefault();
      if (!isStopState && canSend) {
        void handleActionClick();
      }
    }
  };

  const modeStyle = WORK_MODE_STYLES[workMode];
  const permissionTextColor =
    permissionMode === "manual"
      ? "text-[#fac454]"
      : "text-[#8aa8d8]";

  const cardInner = (
    <div className="relative w-full">
      {/* Slash Command Palette (Frame 11) */}
      {slashOpen && (
        <div
          data-testid="slash-command-palette"
          className="absolute bottom-[calc(100%+8px)] left-0 right-0 z-30 rounded-[11px] border border-[#2c2c2c] bg-[#1c1c1c] p-1.5 shadow-[0_-8px_32px_rgba(0,0,0,0.45)]"
        >
          <div className="px-2.5 pb-1.5 pt-1 text-[10.5px] text-[#6e6e6e]">
            Commands
          </div>
          {slashCommands
            .filter((item) =>
              draft.startsWith("/") && draft.length > 1
                ? item.cmd.startsWith(draft.toLowerCase())
                : true
            )
            .map((item, idx) => (
              <button
                key={item.cmd}
                type="button"
                onClick={item.action}
                className={`flex w-full items-baseline gap-2.5 rounded-[7px] px-2.5 py-1.5 text-left text-[12.5px] transition-colors hover:bg-[#262626] ${
                  idx === 0 ? "bg-[#262626]/60" : ""
                }`}
              >
                <span className="min-w-[110px] font-mono text-[#8aa8d8]">
                  {item.cmd}
                </span>
                <span className="text-[12px] text-[#8a8a8a]">{item.desc}</span>
              </button>
            ))}
        </div>
      )}

      {/* Attach Menu (Frame 07) */}
      {activePopover === "attach" && (
        <div
          data-testid="attach-menu"
          className="absolute bottom-[calc(100%+8px)] left-0 z-30 w-[210px] rounded-[11px] border border-[#2c2c2c] bg-[#1d1d1d] p-1.5 shadow-[0_12px_40px_rgba(0,0,0,0.5)]"
        >
          <button
            type="button"
            onClick={() => {
              setActivePopover(null);
              setAttachInputOpen(true);
            }}
            className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[12.5px] text-[#d9d9d9] hover:bg-[#262626]"
          >
            <Paperclip className="h-3.5 w-3.5 text-[#8a8a8a]" />
            <span>Add files or photos</span>
            <span className="ml-auto text-[11px] text-[#6e6e6e]">⌘U</span>
          </button>
          <div className="my-1 h-px bg-[#2a2a2a]" />
          <button
            type="button"
            onClick={() => {
              setActivePopover(null);
              onOpenSettings?.();
            }}
            className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[12.5px] text-[#d9d9d9] hover:bg-[#262626]"
          >
            <FileText className="h-3.5 w-3.5 text-[#8a8a8a]" />
            <span>Skills</span>
            <ChevronRight className="ml-auto h-3 w-3 text-[#6e6e6e]" />
          </button>
          <button
            type="button"
            onClick={() => {
              setActivePopover(null);
              onOpenMcp?.();
            }}
            className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[12.5px] text-[#d9d9d9] hover:bg-[#262626]"
          >
            <Layers className="h-3.5 w-3.5 text-[#8a8a8a]" />
            <span>Connectors</span>
            <ChevronRight className="ml-auto h-3 w-3 text-[#6e6e6e]" />
          </button>
          <button
            type="button"
            onClick={() => {
              setActivePopover(null);
              onOpenSettings?.();
            }}
            className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[12.5px] text-[#d9d9d9] hover:bg-[#262626]"
          >
            <Puzzle className="h-3.5 w-3.5 text-[#8a8a8a]" />
            <span>Add plugins</span>
          </button>
        </div>
      )}

      {/* Model Profile Menu (Frame 13) */}
      {activePopover === "model" && (
        <div
          data-testid="model-profile-menu"
          className="absolute bottom-[calc(100%+12px)] right-16 z-30 w-[240px] rounded-[11px] border border-[#2c2c2c] bg-[#1d1d1d] p-1.5 shadow-[0_12px_40px_rgba(0,0,0,0.5)]"
        >
          <div className="px-2.5 pb-1.5 pt-1 text-[10.5px] text-[#6e6e6e]">
            Model profile
          </div>
          {profiles.length === 0 ? (
            <div className="px-2.5 py-1.5 text-[12px] text-[#8a8a8a]">
              {selectedModel ?? "default"}
            </div>
          ) : (
            profiles.map((prof) => {
              const active = prof.name === selectedProfile;
              return (
                <button
                  key={prof.name}
                  type="button"
                  disabled={runActive}
                  onClick={() => {
                    setActivePopover(null);
                    void onSelectProfile?.(prof.name);
                  }}
                  className={`flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[12.5px] hover:bg-[#262626] disabled:opacity-50 ${
                    active ? "bg-[#262626] text-[#e8e8e8]" : "text-[#c9c9c9]"
                  }`}
                >
                  <Cpu className="h-3.5 w-3.5 shrink-0 text-[#8a8a8a]" />
                  <span className="truncate font-mono text-[12px]">
                    {prof.name}
                  </span>
                  <span className="ml-auto flex items-center gap-1 text-[11px] text-[#6e6e6e]">
                    {prof.model}
                    {active && <Check className="h-3 w-3 text-[#d9a08e]" />}
                  </span>
                </button>
              );
            })
          )}
          <div className="my-1.5 h-px bg-[#2a2a2a]" />
          <button
            type="button"
            onClick={() => {
              setActivePopover(null);
              onOpenSettings?.();
            }}
            className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[12.5px] text-[#d9a08e] hover:bg-[#262626]"
          >
            <SlidersHorizontal className="h-3.5 w-3.5 shrink-0" />
            <span>Manage profiles…</span>
          </button>
        </div>
      )}

      {/* Effort Popover (Frame 13 + cc_eff WebGL2 energy grid) */}
      <div className="absolute bottom-[calc(100%+12px)] right-0 z-30">
        <EffortPopover
          open={activePopover === "effort"}
          effort={thinkingEffort}
          ultracodeEnabled={ultracodeEnabled}
          disabled={runActive}
          onSelectEffort={async (eff, ultra) => {
            if (onSelectEffort) {
              return onSelectEffort(eff, ultra);
            }
            return false;
          }}
          onClose={() => setActivePopover(null)}
        />
      </div>

      {/* Composer Card */}
      <div className="rounded-[14px] border border-[#2b2b2b] bg-[#1d1d1d] px-[14px] py-[12px] shadow-[0_8px_30px_rgba(0,0,0,0.35)]">
        <StagedAttachmentsStrip
          items={stagedAttachments}
          onRemove={(id) => onRemoveAttachment?.(id)}
        />

        {attachInputOpen && (
          <div className="mb-2.5 flex items-center gap-2 rounded-[9px] border border-[#2e2e2e] bg-[#141414] p-2 text-xs">
            <input
              type="text"
              aria-label="Attachment file path"
              value={attachPath}
              disabled={disabled || staging}
              onChange={(e) => setAttachPath(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  void handleStage();
                }
              }}
              placeholder="Local file path inside workspace or readable file..."
              className="flex-1 rounded-[7px] border border-[#2e2e2e] bg-[#1d1d1d] px-2.5 py-1 font-mono text-xs text-[#e8e8e8] placeholder:text-[#6e6e6e] focus:border-[#d9a08e] focus:outline-none"
            />
            <button
              type="button"
              disabled={disabled || staging || attachPath.trim().length === 0}
              onClick={() => void handleStage()}
              className="inline-flex items-center gap-1 rounded-[7px] bg-[#d9a08e] px-2.5 py-1 text-[11px] font-semibold text-[#1c120e] disabled:opacity-40"
            >
              <Plus className="h-3 w-3" />
              Stage
            </button>
          </div>
        )}

        <textarea
          ref={textareaRef}
          aria-label="Prompt input"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={handleKeyDown}
          disabled={disabled}
          rows={variant === "home" ? 2 : 2}
          placeholder={
            approvalPending
              ? "Steer the run, or reply to approval above…"
              : runActive
                ? "Steer the run, or press Esc / Stop…"
                : variant === "home"
                  ? "How can I help you today?"
                  : "Type a message or / for commands…"
          }
          className="max-h-40 min-h-[26px] w-full resize-none bg-transparent text-[13.5px] leading-[1.45] text-[#e8e8e8] placeholder:text-[#6e6e6e] focus:outline-none disabled:opacity-50"
        />

        {/* Dashed Message Preview when staging attachments with draft (Frame 12) */}
        {stagedAttachments.length > 0 && hasInput && (
          <div
            data-testid="composer-message-preview"
            className="mt-2 rounded-[10px] border border-dashed border-[#3a3a3a] px-3 py-2 text-[12px]"
          >
            <div className="mb-1 text-[10px] uppercase tracking-[0.06em] text-[#6e6e6e]">
              Message preview
            </div>
            <div className="text-[#c9c9c9]">
              <MarkdownRenderer content={draft} mode="render" />
            </div>
            <div className="mt-1 text-[11px] text-[#6e6e6e]">
              {stagedAttachments.length} attachment(s) ·{" "}
              {stagedAttachments
                .map((a) => sanitizeDisplayText(a.filename))
                .join(" · ")}
            </div>
          </div>
        )}

        {/* Composer Toolbar */}
        <div className="mt-2.5 flex flex-wrap items-center justify-between gap-2">
          <div className="flex items-center gap-1.5">
            <button
              type="button"
              aria-label="Add context menu"
              disabled={disabled || runActive}
              onClick={() =>
                setActivePopover((p) => (p === "attach" ? null : "attach"))
              }
              className={`inline-flex h-[26px] w-[26px] items-center justify-center rounded-[8px] border text-[#a3a3a3] transition-colors disabled:opacity-40 ${
                activePopover === "attach"
                  ? "border-[#3a3a3a] bg-[#2a2a2a] text-[#e8e8e8]"
                  : "border-transparent hover:bg-[#262626] hover:text-[#e8e8e8]"
              }`}
            >
              <Plus className="h-3.5 w-3.5" />
            </button>

            {onStageAttachments && (
              <button
                type="button"
                onClick={() => setAttachInputOpen((prev) => !prev)}
                disabled={disabled || runActive}
                title="Stage local attachment"
                className="inline-flex h-[26px] items-center gap-1 rounded-[8px] px-2 text-[12px] text-[#a3a3a3] hover:bg-[#262626] hover:text-[#e8e8e8] disabled:opacity-40"
              >
                <Paperclip className="h-3.5 w-3.5" />
                <span>Attach</span>
              </button>
            )}

            <button
              type="button"
              onClick={() => setWebSearchActive((v) => !v)}
              className={`inline-flex h-[26px] items-center gap-1.5 rounded-[8px] border px-2.5 text-[12px] transition-colors ${
                webSearchActive
                  ? "border-[#3a3a3a] bg-[#262626] text-[#e8e8e8]"
                  : "border-[#2e2e2e] bg-[#1c1c1c] text-[#a3a3a3] hover:text-[#e8e8e8]"
              }`}
            >
              <Globe className="h-3 w-3 text-[#8a8a8a]" />
              <span>Web search</span>
            </button>

            <button
              type="button"
              aria-label="Slash commands"
              onClick={() =>
                setActivePopover((p) => (p === "commands" ? null : "commands"))
              }
              className={`inline-flex h-[26px] w-[26px] items-center justify-center rounded-[8px] border transition-colors ${
                slashOpen
                  ? "border-[#3a3a3a] bg-[#262626] text-[#e8e8e8]"
                  : "border-[#2e2e2e] bg-[#1c1c1c] text-[#a3a3a3] hover:text-[#e8e8e8]"
              }`}
            >
              <List className="h-3.5 w-3.5" />
            </button>

            {hasSession && !runActive && (
              <button
                type="button"
                onClick={() => void onCompact()}
                disabled={disabled}
                title="Compact session history"
                className="inline-flex h-[26px] items-center gap-1 rounded-[8px] border border-[#2e2e2e] bg-[#1c1c1c] px-2.5 text-[12px] text-[#c9c9c9] hover:border-[#3a3a3a] disabled:opacity-40"
              >
                <Minimize2 className="h-3 w-3 text-[#8a8a8a]" />
                <span>Compact</span>
              </button>
            )}
          </div>

          <div className="flex flex-wrap items-center gap-1">
            {approvalPending && (
              <span className="devin-pill text-[#fac454]">
                paused · approval required
              </span>
            )}

            <button
              type="button"
              onClick={() => void onCycleMode?.()}
              title="Cycle work mode"
              className="inline-flex h-[26px] items-center gap-1.5 rounded-[7px] px-2 text-[11.5px] text-[#c9c9c9] hover:bg-[#262626]"
            >
              <span
                className="h-1.5 w-1.5 rounded-full"
                style={{ backgroundColor: modeStyle.color }}
              />
              <span>{modeStyle.label}</span>
            </button>

            <button
              type="button"
              onClick={() => onOpenSettings?.()}
              className={`inline-flex h-[26px] items-center rounded-[7px] px-2 text-[11.5px] hover:bg-[#262626] ${permissionTextColor}`}
            >
              {PERMISSION_MODE_LABELS[permissionMode]}
            </button>

            <button
              type="button"
              onClick={() =>
                setActivePopover((p) => (p === "model" ? null : "model"))
              }
              className="inline-flex h-[26px] items-center rounded-[7px] border border-transparent px-2 text-[11.5px] text-[#b9b5ac] hover:border-[#2e2e2e] hover:bg-[#262626]"
            >
              <span className="max-w-[140px] truncate">
                {selectedModel ?? "claude-sonnet-4-5"}
              </span>
            </button>

            {onSelectEffort && (
              <EffortPill
                effort={thinkingEffort}
                ultracodeEnabled={ultracodeEnabled}
                open={activePopover === "effort"}
                disabled={runActive}
                onClick={() =>
                  setActivePopover((p) => (p === "effort" ? null : "effort"))
                }
              />
            )}

            {runActive && (
              <button
                type="button"
                onClick={onToggleSubmitMode}
                title="Switch run guidance mode (Steer / Queue)"
                className="inline-flex h-[26px] items-center gap-1 rounded-[7px] border border-[#2e2e2e] bg-[#1c1c1c] px-2 text-[11px] text-[#c9c9c9] hover:border-[#3a3a3a]"
              >
                <span>{submitMode === "queue" ? "Queue" : "Steer"}</span>
                <ChevronDown className="h-3 w-3 text-[#6e6e6e]" />
              </button>
            )}

            <button
              type="button"
              onClick={() => void handleActionClick()}
              disabled={isStopState ? cancelling : !canSend}
              aria-label={
                isStopState
                  ? cancelling
                    ? "Cancelling"
                    : "Cancel"
                  : "Send"
              }
              title={
                isStopState
                  ? cancelling
                    ? "Cancelling..."
                    : "Stop active run"
                  : runActive
                    ? "Steer active run"
                    : "Send prompt"
              }
              className={`ml-0.5 grid h-[28px] w-[28px] shrink-0 place-items-center rounded-full transition-transform active:scale-95 disabled:cursor-not-allowed ${
                isStopState || canSend
                  ? "bg-[#d9a08e] text-[#1c120e] hover:bg-[#e2a68f]"
                  : "bg-[#2e2e2e] text-[#7a7a7a] opacity-70"
              }`}
            >
              {isStopState ? (
                <Square className="h-2.5 w-2.5 fill-current" />
              ) : (
                <ArrowUp className="h-3.5 w-3.5 stroke-[2.3]" />
              )}
            </button>
          </div>
        </div>
      </div>
    </div>
  );

  if (variant === "home") {
    return <div className="w-full max-w-[620px]">{cardInner}</div>;
  }

  return (
    <div className="bg-[#141414] px-6 pb-3.5 pt-2">
      <div className="mx-auto flex max-w-[840px] flex-col gap-2">
        {cardInner}
        <div className="flex items-center justify-center gap-4 text-[11px] text-[#5e5e5e]">
          <span>Enter send</span>
          <span>Shift+Enter newline</span>
          <span>Empty Tab cycle mode</span>
          <span>Esc cancel</span>
        </div>
      </div>
    </div>
  );
}
