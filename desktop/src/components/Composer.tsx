import {
  ArrowUp,
  Check,
  ChevronDown,
  ChevronRight,
  Cpu,
  FileText,
  FolderOpen,
  Globe,
  Layers,
  List,
  Minimize2,
  Paperclip,
  Plus,
  Puzzle,
  SlidersHorizontal,
  Square,
  X,
} from "lucide-react";
import { isTauri } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { type KeyboardEvent, useEffect, useRef, useState } from "react";
import { commands } from "../bindings";
import type {
  PastedAttachmentItem,
  PermissionModeDto,
  ProfileOptionDto_Serialize,
  StagedAttachmentDto,
  WorkModeDto,
} from "../bindings";
import { getMessages } from "../lib/i18n";
import {
  PERMISSION_MODE_LABELS,
  WORK_MODE_STYLES,
  sanitizeDisplayText,
} from "../lib/sanitize";
import { useAppStore } from "../store/useAppStore";
import { StagedAttachmentsStrip } from "./AttachmentPreview";
import { ContextRing } from "./ContextRing";
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
  queuedPromptPreview?: string | null;
  compactionPending?: boolean;
  contextUsedTokens?: number | null;
  contextWindowTokens?: number | null;
  contextEstimated?: boolean;
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
  onClearQueuedPrompt?: () => void;
  onStageAttachments?: (paths: string[]) => Promise<boolean>;
  onPasteAttachments?: (items: PastedAttachmentItem[]) => Promise<boolean>;
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

type MentionKind = "skill" | "plugin" | "mcp";

interface MentionItem {
  kind: MentionKind;
  name: string;
  desc: string;
}

interface MentionState {
  start: number;
  end: number;
  query: string;
}

const MENTION_ICONS: Record<MentionKind, typeof Layers> = {
  skill: Layers,
  plugin: Puzzle,
  mcp: Cpu,
};

function detectMention(value: string, caret: number): MentionState | null {
  const before = value.slice(0, caret);
  const at = before.lastIndexOf("@");
  if (at < 0) return null;
  if (at > 0 && !/\s/.test(before[at - 1])) return null;
  const query = before.slice(at + 1);
  if (/[\s@]/.test(query)) return null;
  return { start: at, end: caret, query };
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
  queuedPromptPreview = null,
  compactionPending = false,
  contextUsedTokens = null,
  contextWindowTokens = null,
  contextEstimated = false,
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
  onClearQueuedPrompt,
  onStageAttachments,
  onPasteAttachments,
  onRemoveAttachment,
  onOpenSettings,
  onOpenSessions,
  onOpenMemory,
  onOpenMcp,
  onOpenExport,
}: ComposerProps) {
  const locale = useAppStore((s) => s.locale);
  const t = getMessages(locale);
  const [draft, setDraft] = useState("");
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const [sending, setSending] = useState(false);
  const [attachInputOpen, setAttachInputOpen] = useState(false);
  const [attachPath, setAttachPath] = useState("");
  const [staging, setStaging] = useState(false);
  const [webSearchActive, setWebSearchActive] = useState(false);
  const [activePopover, setActivePopover] = useState<
    "attach" | "commands" | "effort" | "model" | null
  >(forcePopover);
  const [mention, setMention] = useState<MentionState | null>(null);
  const [mentionItems, setMentionItems] = useState<MentionItem[] | null>(null);
  const [mentionIndex, setMentionIndex] = useState(0);
  const [dragOver, setDragOver] = useState(false);

  useEffect(() => {
    setActivePopover(forcePopover);
  }, [forcePopover]);

  // Lazily load mentionable items the first time "@" opens the palette.
  useEffect(() => {
    if (!mention || mentionItems !== null || !isTauri()) return;
    let cancelled = false;
    void (async () => {
      const [extRes, mcpRes] = await Promise.all([
        commands.listExtensions(),
        commands.listMcpServers(),
      ]);
      if (cancelled) return;
      const items: MentionItem[] = [];
      if (extRes.status === "ok" && Array.isArray(extRes.data)) {
        for (const ext of extRes.data) {
          if (ext.kind === "skill" || ext.kind === "plugin") {
            items.push({
              kind: ext.kind,
              name: ext.name,
              desc: ext.description,
            });
          }
        }
      }
      if (mcpRes.status === "ok" && Array.isArray(mcpRes.data)) {
        for (const srv of mcpRes.data) {
          items.push({ kind: "mcp", name: srv.name, desc: srv.target });
        }
      }
      setMentionItems(items);
    })();
    return () => {
      cancelled = true;
    };
  }, [mention, mentionItems]);

  useEffect(() => {
    setMentionIndex(0);
  }, [mention?.query]);

  // Files dropped anywhere on the window are staged as attachments.
  useEffect(() => {
    if (!isTauri() || !onStageAttachments) return;
    let unlisten: (() => void) | null = null;
    void getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type === "drop") {
          setDragOver(false);
          if (!disabled && event.payload.paths.length > 0) {
            void onStageAttachments(event.payload.paths);
          }
        } else if (
          event.payload.type === "enter" ||
          event.payload.type === "over"
        ) {
          setDragOver(true);
        } else {
          setDragOver(false);
        }
      })
      .then((f) => {
        unlisten = f;
      });
    return () => unlisten?.();
  }, [onStageAttachments, disabled]);

  // Close lightweight popovers (attach/commands/effort/model, attach input)
  // when pressing outside the composer. Settings dialog is unaffected.
  useEffect(() => {
    if (!activePopover && !attachInputOpen) return;
    const onDown = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setActivePopover(null);
        setAttachInputOpen(false);
      }
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [activePopover, attachInputOpen]);

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

  const mentionMatches = mention
    ? (mentionItems ?? [])
        .filter((it) =>
          it.name.toLowerCase().includes(mention.query.toLowerCase())
        )
        .slice(0, 8)
    : [];
  const mentionActiveIdx =
    mentionMatches.length > 0
      ? Math.min(mentionIndex, mentionMatches.length - 1)
      : 0;

  const slashCommands: SlashCommandItem[] = [
    {
      cmd: "/model",
      desc: t.composer.cmdModel,
      action: () => {
        setDraft("");
        setActivePopover("model");
      },
    },
    {
      cmd: "/mode",
      desc: t.composer.cmdMode,
      action: () => {
        setDraft("");
        setActivePopover(null);
        void onCycleMode?.();
      },
    },
    {
      cmd: "/permission",
      desc: t.composer.cmdPermission,
      action: () => {
        setDraft("");
        setActivePopover(null);
        onOpenSettings?.();
      },
    },
    {
      cmd: "/compact",
      desc: t.composer.cmdCompact,
      action: () => {
        setDraft("");
        setActivePopover(null);
        void onCompact();
      },
    },
    {
      cmd: "/export",
      desc: t.composer.cmdExport,
      action: () => {
        setDraft("");
        setActivePopover(null);
        onOpenExport?.();
      },
    },
    {
      cmd: "/sessions",
      desc: t.composer.cmdSessions,
      action: () => {
        setDraft("");
        setActivePopover(null);
        onOpenSessions?.();
      },
    },
    {
      cmd: "/memory",
      desc: t.composer.cmdMemory,
      action: () => {
        setDraft("");
        setActivePopover(null);
        onOpenMemory?.();
      },
    },
    {
      cmd: "/mcp",
      desc: t.composer.cmdMcp,
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

  // Native file/directory picker (Tauri). Outside the desktop shell
  // (browser dev preview) fall back to the manual path input.
  const handleBrowse = async (directory: boolean) => {
    setActivePopover(null);
    if (!onStageAttachments || disabled) return;
    if (!isTauri()) {
      setAttachInputOpen(true);
      return;
    }
    try {
      const picked = await openDialog(
        directory
          ? { directory: true, multiple: false, title: "Attach folder" }
          : { multiple: true, title: "Attach files" }
      );
      const paths = Array.isArray(picked) ? picked : picked ? [picked] : [];
      if (paths.length === 0) return;
      setStaging(true);
      try {
        await onStageAttachments(paths);
      } finally {
        setStaging(false);
      }
    } catch {
      setAttachInputOpen(true);
    }
  };

  const applyMention = (item: MentionItem) => {
    if (!mention) return;
    const next = `${draft.slice(0, mention.start)}@${item.name} ${draft.slice(mention.end)}`;
    const caret = mention.start + item.name.length + 2;
    setDraft(next);
    setMention(null);
    requestAnimationFrame(() => {
      const el = textareaRef.current;
      el?.focus();
      el?.setSelectionRange(caret, caret);
    });
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
    if (mention) {
      if (event.key === "ArrowDown" && mentionMatches.length > 0) {
        event.preventDefault();
        setMentionIndex((i) => (i + 1) % mentionMatches.length);
        return;
      }
      if (event.key === "ArrowUp" && mentionMatches.length > 0) {
        event.preventDefault();
        setMentionIndex(
          (i) => (i - 1 + mentionMatches.length) % mentionMatches.length
        );
        return;
      }
      if (
        (event.key === "Enter" || event.key === "Tab") &&
        mentionMatches.length > 0
      ) {
        event.preventDefault();
        applyMention(mentionMatches[mentionActiveIdx]);
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        setMention(null);
        return;
      }
    }
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
    <div ref={rootRef} className="relative w-full">
      {/* Slash Command Palette (Frame 11) */}
      {slashOpen && (
        <div
          data-testid="slash-command-palette"
          className="soyo-popover-up absolute bottom-[calc(100%+8px)] left-0 right-0 z-30 rounded-[11px] border border-[#2c2c2c] bg-[#1c1c1c] p-1.5 shadow-[0_-8px_32px_rgba(0,0,0,0.45)]"
        >
          <div className="px-2.5 pb-1.5 pt-1 text-[10.5px] text-[#6e6e6e]">
            {t.composer.commandsHeader}
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

      {/* Mention Palette — "@" references a skill, plugin, or MCP server */}
      {mention && !slashOpen && (
        <div
          data-testid="mention-palette"
          className="soyo-popover-up absolute bottom-[calc(100%+8px)] left-0 right-0 z-30 rounded-[11px] border border-[#2c2c2c] bg-[#1c1c1c] p-1.5 shadow-[0_-8px_32px_rgba(0,0,0,0.45)]"
        >
          <div className="px-2.5 pb-1.5 pt-1 text-[10.5px] text-[#6e6e6e]">
            {t.composer.mentionHeader}
          </div>
          {mentionItems !== null && mentionMatches.length === 0 ? (
            <div className="px-2.5 py-1.5 text-[12px] text-[#6e6e6e]">
              {t.composer.mentionEmpty}
            </div>
          ) : (
            mentionMatches.map((item, idx) => {
              const Icon = MENTION_ICONS[item.kind];
              const kindLabel =
                item.kind === "skill"
                  ? t.composer.mentionSkill
                  : item.kind === "plugin"
                    ? t.composer.mentionPlugin
                    : t.composer.mentionMcp;
              return (
                <button
                  key={`${item.kind}:${item.name}`}
                  type="button"
                  onMouseDown={(e) => {
                    e.preventDefault();
                    applyMention(item);
                  }}
                  className={`flex w-full items-center gap-2.5 rounded-[7px] px-2.5 py-1.5 text-left text-[12.5px] transition-colors hover:bg-[#262626] ${
                    idx === mentionActiveIdx ? "bg-[#262626]/60" : ""
                  }`}
                >
                  <Icon className="h-3.5 w-3.5 shrink-0 text-[#8a8a8a]" />
                  <span className="font-mono text-[#8aa8d8]">
                    @{item.name}
                  </span>
                  <span className="truncate text-[12px] text-[#8a8a8a]">
                    {item.desc}
                  </span>
                  <span className="ml-auto shrink-0 text-[10px] uppercase tracking-[0.05em] text-[#6e6e6e]">
                    {kindLabel}
                  </span>
                </button>
              );
            })
          )}
        </div>
      )}

      {/* Drag-over drop hint */}
      {dragOver && (
        <div
          data-testid="composer-drop-overlay"
          className="pointer-events-none absolute inset-0 z-40 flex items-center justify-center rounded-[14px] border border-dashed border-[#d9a08e] bg-[#181818]/85 text-[12px] font-medium text-[#d9a08e]"
        >
          {t.composer.dropHint}
        </div>
      )}

      {/* Attach Menu (Frame 07) */}
      {activePopover === "attach" && (
        <div
          data-testid="attach-menu"
          className="soyo-popover-up absolute bottom-[calc(100%+8px)] left-0 z-30 w-[210px] rounded-[11px] border border-[#2c2c2c] bg-[#1d1d1d] p-1.5 shadow-[0_12px_40px_rgba(0,0,0,0.5)]"
        >
          <button
            type="button"
            onClick={() => void handleBrowse(false)}
            className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[12.5px] text-[#d9d9d9] transition-colors hover:bg-[#262626]"
          >
            <Paperclip className="h-3.5 w-3.5 text-[#8a8a8a]" />
            <span>{t.composer.addFilesOrPhotos}</span>
            <span className="ml-auto text-[11px] text-[#6e6e6e]">⌘U</span>
          </button>
          <button
            type="button"
            onClick={() => void handleBrowse(true)}
            className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[12.5px] text-[#d9d9d9] transition-colors hover:bg-[#262626]"
          >
            <FolderOpen className="h-3.5 w-3.5 text-[#8a8a8a]" />
            <span>{t.composer.addFolder}</span>
          </button>
          <button
            type="button"
            onClick={() => {
              setActivePopover(null);
              setAttachInputOpen(true);
            }}
            className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[12.5px] text-[#d9d9d9] transition-colors hover:bg-[#262626]"
          >
            <FileText className="h-3.5 w-3.5 text-[#8a8a8a]" />
            <span>{t.composer.enterPathManually}</span>
          </button>
          <div className="my-1 h-px bg-[#2a2a2a]" />
          <button
            type="button"
            onClick={() => {
              setActivePopover(null);
              onOpenSettings?.();
            }}
            className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[12.5px] text-[#d9d9d9] transition-colors hover:bg-[#262626]"
          >
            <FileText className="h-3.5 w-3.5 text-[#8a8a8a]" />
            <span>{t.composer.skills}</span>
            <ChevronRight className="ml-auto h-3 w-3 text-[#6e6e6e]" />
          </button>
          <button
            type="button"
            onClick={() => {
              setActivePopover(null);
              onOpenMcp?.();
            }}
            className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[12.5px] text-[#d9d9d9] transition-colors hover:bg-[#262626]"
          >
            <Layers className="h-3.5 w-3.5 text-[#8a8a8a]" />
            <span>{t.composer.connectors}</span>
            <ChevronRight className="ml-auto h-3 w-3 text-[#6e6e6e]" />
          </button>
          <button
            type="button"
            onClick={() => {
              setActivePopover(null);
              onOpenSettings?.();
            }}
            className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[12.5px] text-[#d9d9d9] transition-colors hover:bg-[#262626]"
          >
            <Puzzle className="h-3.5 w-3.5 text-[#8a8a8a]" />
            <span>{t.composer.addPlugins}</span>
          </button>
        </div>
      )}

      {/* Model Profile Menu (Frame 13) */}
      {activePopover === "model" && (
        <div
          data-testid="model-profile-menu"
          className="soyo-popover-up absolute bottom-[calc(100%+12px)] right-16 z-30 w-[240px] rounded-[11px] border border-[#2c2c2c] bg-[#1d1d1d] p-1.5 shadow-[0_12px_40px_rgba(0,0,0,0.5)]"
        >
          <div className="px-2.5 pb-1.5 pt-1 text-[10.5px] text-[#6e6e6e]">
            {t.composer.modelProfileHeader}
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
                  className={`flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[12.5px] transition-colors hover:bg-[#262626] disabled:opacity-50 ${
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
            className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[12.5px] text-[#d9a08e] transition-colors hover:bg-[#262626]"
          >
            <SlidersHorizontal className="h-3.5 w-3.5 shrink-0" />
            <span>{t.composer.manageProfiles}</span>
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
      <div className="soyo-composer-card rounded-[14px] border border-[#2b2b2b] bg-[#1d1d1d] px-[14px] py-[12px] shadow-[0_8px_30px_rgba(0,0,0,0.35)]">
        <StagedAttachmentsStrip
          items={stagedAttachments}
          onRemove={(id) => onRemoveAttachment?.(id)}
        />

        {attachInputOpen && (
          <div className="soyo-panel-enter mb-2.5 flex items-center gap-2 rounded-[9px] border border-[#2e2e2e] bg-[#141414] p-2 text-xs">
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
              placeholder={t.composer.stagePathPlaceholder}
              className="soyo-input flex-1 px-2.5 py-1 font-mono text-xs"
            />
            <button
              type="button"
              disabled={disabled || staging || attachPath.trim().length === 0}
              onClick={() => void handleStage()}
              className="inline-flex items-center gap-1 rounded-[7px] bg-[#d9a08e] px-2.5 py-1 text-[11px] font-semibold text-[#1c120e] transition-colors hover:bg-[#e2a68f] disabled:opacity-40"
            >
              <Plus className="h-3 w-3" />
              {t.composer.stageBtn}
            </button>
          </div>
        )}

        <textarea
          ref={textareaRef}
          aria-label="Prompt input"
          value={draft}
          onChange={(e) => {
            setDraft(e.target.value);
            setMention(
              detectMention(
                e.target.value,
                e.target.selectionStart ?? e.target.value.length
              )
            );
          }}
          onPaste={(e) => {
            // Clipboard files (screenshots, copied files) carry no usable
            // path in a webview — stage their bytes via the paste pipeline.
            const files = Array.from(e.clipboardData?.files ?? []);
            if (
              files.length === 0 ||
              !onPasteAttachments ||
              disabled ||
              !isTauri()
            ) {
              return;
            }
            e.preventDefault();
            setStaging(true);
            void (async () => {
              const items = await Promise.all(
                files.map(async (f, i) => ({
                  name: f.name || `pasted-${i + 1}.png`,
                  data: Array.from(new Uint8Array(await f.arrayBuffer())),
                }))
              );
              await onPasteAttachments(items);
            })()
              .catch(() => {})
              .finally(() => setStaging(false));
          }}
          onKeyDown={handleKeyDown}
          disabled={disabled}
          rows={variant === "home" ? 2 : 2}
          placeholder={
            approvalPending
              ? t.composer.placeholderApproval
              : runActive
                ? t.composer.placeholderActive
                : variant === "home"
                  ? t.composer.placeholderHome
                  : t.composer.placeholderDefault
          }
          className="max-h-40 min-h-[26px] w-full resize-none bg-transparent text-[13.5px] leading-[1.45] text-[#e8e8e8] placeholder:text-[#6e6e6e] focus:outline-none disabled:opacity-50"
        />

        {/* Dashed Message Preview when staging attachments with draft (Frame 12) */}
        {stagedAttachments.length > 0 && hasInput && (
          <div
            data-testid="composer-message-preview"
            className="soyo-panel-enter mt-2 rounded-[10px] border border-dashed border-[#3a3a3a] px-3 py-2 text-[12px]"
          >
            <div className="mb-1 text-[10px] uppercase tracking-[0.06em] text-[#6e6e6e]">
              {t.composer.messagePreview}
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

        {/* Composer Toolbar — Frame 01 home keeps a single minimal row (+ · model · send) */}
        <div className="mt-2.5 flex items-center justify-between gap-2">
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

            {variant !== "home" && onStageAttachments && (
              <button
                type="button"
                onClick={() => setAttachInputOpen((prev) => !prev)}
                disabled={disabled || runActive}
                title="Stage local attachment"
                className="inline-flex h-[26px] items-center gap-1 rounded-[8px] px-2 text-[12px] text-[#a3a3a3] hover:bg-[#262626] hover:text-[#e8e8e8] disabled:opacity-40"
              >
                <Paperclip className="h-3.5 w-3.5" />
                <span>{t.composer.attach}</span>
              </button>
            )}

            {variant !== "home" && (
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
              <span>{t.composer.webSearch}</span>
            </button>
            )}

            {variant !== "home" && (
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
            )}

            {variant !== "home" && hasSession && !runActive && (
              <button
                type="button"
                onClick={() => void onCompact()}
                disabled={disabled}
                title="Compact session history"
                className="inline-flex h-[26px] items-center gap-1 rounded-[8px] border border-[#2e2e2e] bg-[#1c1c1c] px-2.5 text-[12px] text-[#c9c9c9] hover:border-[#3a3a3a] disabled:opacity-40"
              >
                <Minimize2 className="h-3 w-3 text-[#8a8a8a]" />
                <span>{t.composer.compact}</span>
              </button>
            )}
          </div>

          <div className="flex items-center gap-1">
            {approvalPending && (
              <span className="soyo-pill text-[#fac454]">
                {t.composer.pausedApproval}
              </span>
            )}

            {variant !== "home" && (
              <>
                <button
                  type="button"
                  onClick={() => void onCycleMode?.()}
                  title="Cycle work mode"
                  className="inline-flex h-[26px] items-center gap-1.5 rounded-[7px] px-2 text-[11.5px] text-[#c9c9c9] transition-colors hover:bg-[#262626]"
                >
                  <span
                    className="h-1.5 w-1.5 rounded-full transition-colors"
                    style={{ backgroundColor: modeStyle.color }}
                  />
                  <span>{modeStyle.label}</span>
                </button>

                <button
                  type="button"
                  onClick={() => onOpenSettings?.()}
                  className={`inline-flex h-[26px] items-center rounded-[7px] px-2 text-[11.5px] transition-colors hover:bg-[#262626] ${permissionTextColor}`}
                >
                  {PERMISSION_MODE_LABELS[permissionMode]}
                </button>
              </>
            )}

            <button
              type="button"
              onClick={() =>
                setActivePopover((p) => (p === "model" ? null : "model"))
              }
              className="inline-flex h-[26px] items-center rounded-[7px] border border-transparent px-2 text-[11.5px] text-[#a3a3a3] transition-colors hover:border-[#2e2e2e] hover:bg-[#262626] hover:text-[#e8e8e8]"
            >
              <span className="max-w-[140px] truncate">
                {selectedModel ?? "claude-sonnet-4-5"}
                {variant === "home" && thinkingEffort
                  ? ` · ${thinkingEffort.charAt(0).toUpperCase()}${thinkingEffort.slice(1)}`
                  : ""}
              </span>
            </button>

            {variant !== "home" && onSelectEffort && (
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
                className="inline-flex h-[26px] items-center gap-1 rounded-[7px] border border-[#2e2e2e] bg-[#1c1c1c] px-2 text-[11px] text-[#c9c9c9] transition-colors hover:border-[#3a3a3a]"
              >
                <span>{submitMode === "queue" ? t.composer.queue : t.composer.steer}</span>
                <ChevronDown className="h-3 w-3 text-[#6e6e6e]" />
              </button>
            )}

            {compactionPending && !queuedPromptPreview && (
              <span
                title={t.composer.compactionQueued}
                className="inline-flex h-[26px] items-center gap-1 rounded-[7px] border border-[#3a3a3a] bg-[#1c1c1c] px-2 text-[11px] text-[#8aa8d8]"
              >
                <Minimize2 className="h-3 w-3" />
                <span>{t.composer.compactionQueued}</span>
              </span>
            )}

            {queuedPromptPreview && (
              <button
                type="button"
                onClick={onClearQueuedPrompt}
                title={`${t.composer.queuedPrompt}: ${queuedPromptPreview}`}
                className="inline-flex h-[26px] max-w-[140px] items-center gap-1 rounded-[7px] border border-[#3a3a3a] bg-[#242019] px-2 text-[11px] text-[#fac454] transition-colors hover:border-[#4a4a3a]"
              >
                <span className="truncate">{t.composer.queuedPrompt}</span>
                <X className="h-3 w-3 shrink-0" />
              </button>
            )}

            {variant !== "home" && contextWindowTokens != null && (
              <ContextRing
                usedTokens={contextUsedTokens}
                windowTokens={contextWindowTokens}
                estimated={contextEstimated}
                size={19}
              />
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
              className={`ml-0.5 grid h-[28px] w-[28px] shrink-0 place-items-center rounded-full transition-all duration-200 ease-out active:scale-95 disabled:cursor-not-allowed ${
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
          <span>{t.composer.hintEnter}</span>
          <span>{t.composer.hintShiftEnter}</span>
          <span>{t.composer.hintTab}</span>
          <span>{t.composer.hintEsc}</span>
        </div>
      </div>
    </div>
  );
}
