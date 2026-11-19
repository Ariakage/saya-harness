import {
  ArrowUp,
  AtSign,
  Check,
  ChevronDown,
  ChevronRight,
  ClipboardPaste,
  Cpu,
  FileText,
  FolderOpen,
  Gauge,
  Globe,
  Hand,
  Layers,
  List,
  Minimize2,
  Paperclip,
  Plus,
  Puzzle,
  ShieldAlert,
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
import { SquaresLoader } from "./SquaresLoader";
import { Button } from "./ui/Button";
import { Pill } from "./ui/Pill";

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
  onOpenPermissions?: () => void;
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
  onOpenPermissions,
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
  const [narrowComposer, setNarrowComposer] = useState(false);
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

  useEffect(() => {
    const el = rootRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const target =
      (el.closest("[data-main-pane]") as HTMLElement | null) ??
      el.parentElement ??
      el;
    const updateWidth = () => {
      const w = target.clientWidth;
      setNarrowComposer(w > 0 && w < 640);
    };
    updateWidth();
    const ro = new ResizeObserver(() => updateWidth());
    ro.observe(target);
    return () => ro.disconnect();
  }, []);

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
          ? {
              directory: true,
              multiple: false,
              title: t.composer.addFolder,
            }
          : { multiple: true, title: t.composer.addFilesOrPhotos }
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

  const insertMentionTrigger = () => {
    const el = textareaRef.current;
    const start = el?.selectionStart ?? draft.length;
    const end = el?.selectionEnd ?? draft.length;
    const prefix = start > 0 && !/\s$/.test(draft.slice(0, start)) ? " @" : "@";
    const next = `${draft.slice(0, start)}${prefix}${draft.slice(end)}`;
    const caret = start + prefix.length;
    setDraft(next);
    setMention(detectMention(next, caret));
    setActivePopover(null);
    requestAnimationFrame(() => {
      textareaRef.current?.focus();
      textareaRef.current?.setSelectionRange(caret, caret);
    });
  };

  const handlePasteAction = async () => {
    setActivePopover(null);
    try {
      if (typeof navigator !== "undefined" && navigator.clipboard?.readText) {
        const text = await navigator.clipboard.readText();
        if (text) {
          const el = textareaRef.current;
          const start = el?.selectionStart ?? draft.length;
          const end = el?.selectionEnd ?? draft.length;
          const next = `${draft.slice(0, start)}${text}${draft.slice(end)}`;
          setDraft(next);
        }
      }
    } catch {
      // Clipboard read may require permission; fall back to focusing textarea.
    } finally {
      textareaRef.current?.focus();
    }
  };

  const modeStyle = WORK_MODE_STYLES[workMode];
  const fullModelName = selectedModel ?? "claude-sonnet-4-5";
  const compactModelName =
    fullModelName.length > 16
      ? `${fullModelName.slice(0, 16)}…`
      : fullModelName;
  const normalizedEffortLabel = thinkingEffort
    ? `${thinkingEffort.charAt(0).toUpperCase()}${thinkingEffort.slice(1)}`
    : null;
  const openPermissionsHandler = onOpenPermissions ?? onOpenSettings;
  const permissionTextColor =
    permissionMode === "bypass"
      ? "text-[var(--danger)]"
      : permissionMode === "manual"
        ? "text-[var(--info)]"
        : "text-[var(--text-secondary)]";

  const cardInner = (
    <div ref={rootRef} className="relative w-full">
      {/* Slash Command Palette */}
      {slashOpen && (
        <div
          data-testid="slash-command-palette"
          className="soyo-popover-up absolute bottom-[calc(100%+8px)] left-0 right-0 z-30 rounded-[var(--radius-lg)] border border-[var(--border)] bg-[var(--surface-overlay)] p-1.5 shadow-[var(--shadow-popover)]"
        >
          <div className="px-2.5 pb-1.5 pt-1 text-[11px] text-[var(--text-tertiary)]">
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
                className={`flex w-full items-baseline gap-2.5 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[13px] transition-colors hover:bg-[var(--surface-hover)] ${
                  idx === 0 ? "bg-[var(--surface-raised)]" : ""
                }`}
              >
                <span className="min-w-[110px] font-mono text-[var(--info)]">
                  {item.cmd}
                </span>
                <span className="text-[12px] text-[var(--text-secondary)]">
                  {item.desc}
                </span>
              </button>
            ))}
        </div>
      )}

      {/* Mention Palette */}
      {mention && !slashOpen && (
        <div
          data-testid="mention-palette"
          className="soyo-popover-up absolute bottom-[calc(100%+8px)] left-0 right-0 z-30 rounded-[var(--radius-lg)] border border-[var(--border)] bg-[var(--surface-overlay)] p-1.5 shadow-[var(--shadow-popover)]"
        >
          <div className="px-2.5 pb-1.5 pt-1 text-[11px] text-[var(--text-tertiary)]">
            {t.composer.mentionHeader}
          </div>
          {mentionItems !== null && mentionMatches.length === 0 ? (
            <div className="px-2.5 py-1.5 text-[12px] text-[var(--text-tertiary)]">
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
                  className={`flex w-full items-center gap-2.5 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[13px] transition-colors hover:bg-[var(--surface-hover)] ${
                    idx === mentionActiveIdx ? "bg-[var(--surface-raised)]" : ""
                  }`}
                >
                  <Icon
                    className="h-3.5 w-3.5 shrink-0 text-[var(--text-tertiary)]"
                    strokeWidth={1.75}
                  />
                  <span className="font-mono text-[var(--info)]">
                    @{item.name}
                  </span>
                  <span className="truncate text-[12px] text-[var(--text-secondary)]">
                    {item.desc}
                  </span>
                  <span className="ml-auto shrink-0 text-[11px] uppercase tracking-[0.05em] text-[var(--text-tertiary)]">
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
          className="pointer-events-none absolute inset-0 z-40 flex items-center justify-center rounded-[var(--radius-xl)] border border-dashed border-[var(--accent)] bg-[var(--scrim)] text-[12px] font-medium text-[var(--accent-text)]"
        >
          {t.composer.dropHint}
        </div>
      )}

      {/* Left `+` Context & Tool Menu */}
      {activePopover === "attach" && (
        <div
          data-testid="attach-menu"
          className="soyo-popover-up absolute bottom-[calc(100%+8px)] left-0 z-30 w-[230px] rounded-[var(--radius-lg)] border border-[var(--border)] bg-[var(--surface-overlay)] p-1.5 shadow-[var(--shadow-popover)]"
        >
          {onStageAttachments && (
            <>
              <button
                type="button"
                onClick={() => void handleBrowse(false)}
                className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[13px] text-[var(--text-primary)] transition-colors hover:bg-[var(--surface-hover)]"
              >
                <Paperclip className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                <span>{t.composer.addFilesOrPhotos}</span>
                <span className="ml-auto text-[11px] text-[var(--text-tertiary)]">
                  ⌘U
                </span>
              </button>
              <button
                type="button"
                onClick={() => void handleBrowse(true)}
                className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[13px] text-[var(--text-primary)] transition-colors hover:bg-[var(--surface-hover)]"
              >
                <FolderOpen className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                <span>{t.composer.addFolder}</span>
              </button>
              <button
                type="button"
                onClick={() => {
                  setActivePopover(null);
                  setAttachInputOpen(true);
                }}
                className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[13px] text-[var(--text-primary)] transition-colors hover:bg-[var(--surface-hover)]"
              >
                <FileText className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                <span>{t.composer.enterPathManually}</span>
              </button>
            </>
          )}
          <button
            type="button"
            onClick={() => void handlePasteAction()}
            className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[13px] text-[var(--text-primary)] transition-colors hover:bg-[var(--surface-hover)]"
          >
            <ClipboardPaste className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
            <span>{t.extra.pasteAction}</span>
          </button>
          <button
            type="button"
            onClick={insertMentionTrigger}
            className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[13px] text-[var(--text-primary)] transition-colors hover:bg-[var(--surface-hover)]"
          >
            <AtSign className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
            <span>{t.extra.mentionAction}</span>
          </button>
          <button
            type="button"
            onClick={() => setActivePopover("commands")}
            className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[13px] text-[var(--text-primary)] transition-colors hover:bg-[var(--surface-hover)]"
          >
            <List className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
            <span>{t.extra.slashAction}</span>
          </button>
          {variant !== "home" && hasSession && !runActive && (
            <button
              type="button"
              disabled={disabled}
              onClick={() => {
                setActivePopover(null);
                void onCompact();
              }}
              className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[13px] text-[var(--text-primary)] transition-colors hover:bg-[var(--surface-hover)] disabled:opacity-40"
            >
              <Minimize2 className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
              <span>{t.composer.compact}</span>
            </button>
          )}
          {variant !== "home" && (
            <button
              type="button"
              onClick={() => {
                setWebSearchActive((v) => !v);
                setActivePopover(null);
              }}
              className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[13px] text-[var(--text-primary)] transition-colors hover:bg-[var(--surface-hover)]"
            >
              <Globe className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
              <span>{t.composer.webSearch}</span>
              {webSearchActive && (
                <Check className="ml-auto h-3.5 w-3.5 text-[var(--accent-text)]" strokeWidth={1.75} />
              )}
            </button>
          )}
          <div className="my-1 h-px bg-[var(--border-subtle)]" />
          <button
            type="button"
            onClick={() => {
              setActivePopover(null);
              onOpenSettings?.();
            }}
            className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[13px] text-[var(--text-primary)] transition-colors hover:bg-[var(--surface-hover)]"
          >
            <FileText className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
            <span>{t.composer.skills}</span>
            <ChevronRight className="ml-auto h-3 w-3 text-[var(--text-tertiary)]" strokeWidth={1.75} />
          </button>
          <button
            type="button"
            onClick={() => {
              setActivePopover(null);
              onOpenMcp?.();
            }}
            className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[13px] text-[var(--text-primary)] transition-colors hover:bg-[var(--surface-hover)]"
          >
            <Layers className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
            <span>{t.composer.connectors}</span>
            <ChevronRight className="ml-auto h-3 w-3 text-[var(--text-tertiary)]" strokeWidth={1.75} />
          </button>
          <button
            type="button"
            onClick={() => {
              setActivePopover(null);
              onOpenSettings?.();
            }}
            className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[13px] text-[var(--text-primary)] transition-colors hover:bg-[var(--surface-hover)]"
          >
            <Puzzle className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
            <span>{t.composer.addPlugins}</span>
          </button>
        </div>
      )}

      {/* Model Profile Menu */}
      {activePopover === "model" && (
        <div
          data-testid="model-profile-menu"
          className="soyo-popover-up absolute bottom-[calc(100%+12px)] right-12 z-30 w-[250px] rounded-[var(--radius-lg)] border border-[var(--border)] bg-[var(--surface-overlay)] p-1.5 shadow-[var(--shadow-popover)]"
        >
          <div className="px-2.5 pb-1.5 pt-1 text-[11px] text-[var(--text-tertiary)]">
            {t.composer.modelProfileHeader}
          </div>
          {profiles.length === 0 ? (
            <div className="px-2.5 py-1.5 text-[12px] text-[var(--text-secondary)]">
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
                  className={`flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[13px] transition-colors hover:bg-[var(--surface-hover)] disabled:opacity-50 ${
                    active
                      ? "bg-[var(--surface-raised)] text-[var(--text-primary)]"
                      : "text-[var(--text-secondary)]"
                  }`}
                >
                  <Cpu className="h-3.5 w-3.5 shrink-0 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                  <span className="truncate font-mono text-[12px]">
                    {prof.name}
                  </span>
                  <span className="ml-auto flex items-center gap-1 text-[11px] text-[var(--text-tertiary)]">
                    {prof.model}
                    {active && (
                      <Check className="h-3 w-3 text-[var(--accent-text)]" strokeWidth={1.75} />
                    )}
                  </span>
                </button>
              );
            })
          )}
          {narrowComposer && variant !== "home" && onSelectEffort && (
            <>
              <div className="my-1.5 h-px bg-[var(--border-subtle)]" />
              <button
                type="button"
                disabled={runActive}
                onClick={() => setActivePopover("effort")}
                className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[13px] text-[var(--text-primary)] transition-colors hover:bg-[var(--surface-hover)] disabled:opacity-40"
              >
                <Gauge className="h-3.5 w-3.5 shrink-0 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                <span>{t.extra.selectEffortHeader}</span>
                {normalizedEffortLabel && (
                  <span className="ml-auto text-[11px] text-[var(--text-tertiary)]">
                    {ultracodeEnabled
                      ? t.effort.levels.ultracode.name
                      : normalizedEffortLabel}
                  </span>
                )}
              </button>
            </>
          )}
          <div className="my-1.5 h-px bg-[var(--border-subtle)]" />
          <button
            type="button"
            onClick={() => {
              setActivePopover(null);
              onOpenSettings?.();
            }}
            className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[13px] text-[var(--accent-text)] transition-colors hover:bg-[var(--surface-hover)]"
          >
            <SlidersHorizontal className="h-3.5 w-3.5 shrink-0" strokeWidth={1.75} />
            <span>{t.composer.manageProfiles}</span>
          </button>
        </div>
      )}

      {/* Effort Popover */}
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

      {/* Single 16px-radius Composer Card (§8.6) */}
      <div className="soyo-composer-card rounded-[var(--radius-xl)] border border-[var(--border)] bg-[var(--surface-raised)] px-[14px] py-[12px] shadow-[var(--shadow-popover)]">
        <StagedAttachmentsStrip
          items={stagedAttachments}
          onRemove={(id) => onRemoveAttachment?.(id)}
        />

        {attachInputOpen && (
          <div className="soyo-panel-enter mb-2.5 flex items-center gap-2 rounded-[var(--radius-md)] border border-[var(--border)] bg-[var(--surface-app)] p-2 text-xs">
            <input
              type="text"
              aria-label={t.a11y.attachmentPathInput}
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
            <Button
              variant="primary"
              size="sm"
              disabled={disabled || staging || attachPath.trim().length === 0}
              onClick={() => void handleStage()}
            >
              {staging ? (
                <SquaresLoader size={12} />
              ) : (
                <Plus className="h-3 w-3" strokeWidth={1.75} />
              )}
              {t.composer.stageBtn}
            </Button>
          </div>
        )}

        <textarea
          ref={textareaRef}
          aria-label={t.a11y.promptInput}
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
          rows={2}
          placeholder={
            approvalPending
              ? t.composer.placeholderApproval
              : runActive
                ? t.composer.placeholderActive
                : variant === "home"
                  ? t.composer.placeholderHome
                  : t.composer.placeholderDefault
          }
          className="max-h-40 min-h-[26px] w-full resize-none bg-transparent text-[14px] leading-[1.45] text-[var(--text-primary)] placeholder:text-[var(--text-tertiary)] focus:outline-none disabled:opacity-50"
        />

        {/* Message Preview when staging attachments with draft */}
        {stagedAttachments.length > 0 && hasInput && (
          <div
            data-testid="composer-message-preview"
            className="soyo-panel-enter mt-2 rounded-[var(--radius-md)] border border-dashed border-[var(--border-strong)] px-3 py-2 text-[12px]"
          >
            <div className="mb-1 text-[11px] uppercase tracking-[0.06em] text-[var(--text-tertiary)]">
              {t.composer.messagePreview}
            </div>
            <div className="text-[var(--text-secondary)]">
              <MarkdownRenderer content={draft} mode="render" />
            </div>
            <div className="mt-1 text-[11px] text-[var(--text-tertiary)]">
              {t.cards.attachmentsHeader(stagedAttachments.length)} ·{" "}
              {stagedAttachments
                .map((a) => sanitizeDisplayText(a.filename))
                .join(" · ")}
            </div>
          </div>
        )}

        {/* Single-row non-wrapping Toolbar */}
        <div
          data-testid="composer-toolbar"
          className="mt-2.5 flex items-center justify-between gap-2 overflow-hidden"
        >
          <div className="flex min-w-0 items-center gap-1.5">
            <button
              type="button"
              aria-label={t.a11y.addContextMenu}
              disabled={disabled || runActive}
              onClick={() =>
                setActivePopover((p) => (p === "attach" ? null : "attach"))
              }
              className={`inline-flex h-[28px] w-[28px] shrink-0 items-center justify-center whitespace-nowrap rounded-[var(--radius-md)] border text-[var(--text-secondary)] transition-colors disabled:opacity-40 ${
                activePopover === "attach"
                  ? "border-[var(--border-strong)] bg-[var(--surface-hover)] text-[var(--text-primary)]"
                  : "border-transparent hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
              }`}
            >
              <Plus className="h-3.5 w-3.5" strokeWidth={1.75} />
            </button>

            {webSearchActive && (
              <span
                data-testid="active-web-search-chip"
                className="inline-flex h-[26px] shrink-0 items-center gap-1 whitespace-nowrap rounded-[var(--radius-md)] border border-[var(--border-strong)] bg-[var(--surface-hover)] px-2 text-[12px] text-[var(--text-primary)]"
              >
                <Globe className="h-3 w-3 text-[var(--accent-text)]" strokeWidth={1.75} />
                <span>{t.composer.webSearch}</span>
                <button
                  type="button"
                  onClick={() => setWebSearchActive(false)}
                  aria-label={t.a11y.removeToolChip(t.composer.webSearch)}
                  className="rounded p-0.5 text-[var(--text-tertiary)] hover:text-[var(--text-primary)]"
                >
                  <X className="h-3 w-3" strokeWidth={1.75} />
                </button>
              </span>
            )}

            {(runActive || staging) && (
              <span
                data-testid="composer-working-loader"
                className="inline-flex h-[26px] shrink-0 items-center gap-1.5 whitespace-nowrap px-1 text-[11px] text-[var(--text-tertiary)]"
              >
                <SquaresLoader size={12} />
                {!narrowComposer && (
                  <span>{runActive ? t.home.streaming : t.composer.stageBtn}</span>
                )}
              </span>
            )}
          </div>

          <div className="flex shrink-0 items-center gap-1.5 whitespace-nowrap">
            {approvalPending && (
              <Pill tone="warning" className="shrink-0 whitespace-nowrap">
                {t.composer.pausedApproval}
              </Pill>
            )}

            {(variant !== "home" || permissionMode === "bypass") && (
              <div
                className={`inline-flex h-[28px] shrink-0 items-center overflow-hidden rounded-[var(--radius-md)] border bg-[var(--surface-raised)] whitespace-nowrap ${
                  permissionMode === "bypass"
                    ? "border-[var(--danger)]"
                    : "border-[var(--border)]"
                }`}
              >
                {variant !== "home" && (
                  <>
                    <button
                      type="button"
                      onClick={() => void onCycleMode?.()}
                      title={t.a11y.cycleWorkMode}
                      className="inline-flex h-full shrink-0 items-center gap-1.5 whitespace-nowrap px-2 text-[12px] text-[var(--text-secondary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                    >
                      <span
                        className="h-1.5 w-1.5 shrink-0 rounded-full transition-colors"
                        style={{ backgroundColor: modeStyle.color }}
                      />
                      <span>{modeStyle.label}</span>
                    </button>
                    <span className="h-3.5 w-px shrink-0 bg-[var(--border)]" aria-hidden="true" />
                  </>
                )}

                <button
                  type="button"
                  data-testid={
                    permissionMode === "bypass" ? "composer-bypass-badge" : undefined
                  }
                  onClick={() => openPermissionsHandler?.()}
                  className={`inline-flex h-full shrink-0 items-center gap-1 whitespace-nowrap px-2 text-[12px] transition-colors ${
                    permissionMode === "bypass"
                      ? "bg-[var(--danger-surface)] font-medium text-[var(--danger)]"
                      : `${permissionTextColor} hover:bg-[var(--surface-hover)]`
                  }`}
                >
                  {permissionMode === "bypass" && (
                    <ShieldAlert className="h-3 w-3 shrink-0" strokeWidth={1.75} />
                  )}
                  {permissionMode === "manual" && (
                    <Hand className="h-3 w-3 shrink-0" strokeWidth={1.75} />
                  )}
                  <span>{PERMISSION_MODE_LABELS[permissionMode]}</span>
                </button>
              </div>
            )}

            {/* Merged Model · Effort Capsule */}
            <div className="inline-flex h-[28px] shrink-0 items-center overflow-hidden rounded-[var(--radius-md)] border border-[var(--border)] bg-[var(--surface-raised)] whitespace-nowrap">
              <button
                type="button"
                aria-label={t.a11y.modelAndEffort}
                title={
                  normalizedEffortLabel
                    ? `${fullModelName} · ${
                        ultracodeEnabled
                          ? t.effort.levels.ultracode.name
                          : normalizedEffortLabel
                      }`
                    : fullModelName
                }
                onClick={() =>
                  setActivePopover((p) => (p === "model" ? null : "model"))
                }
                className="inline-flex h-full shrink-0 items-center whitespace-nowrap px-2 text-[12px] text-[var(--text-secondary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
              >
                <span
                  className={
                    narrowComposer
                      ? "max-w-[160px] truncate"
                      : "max-w-[260px] truncate"
                  }
                >
                  {narrowComposer ? compactModelName : fullModelName}
                  {variant === "home" && !narrowComposer && normalizedEffortLabel
                    ? ` · ${normalizedEffortLabel}`
                    : ""}
                </span>
              </button>

              {!narrowComposer && variant !== "home" && onSelectEffort && (
                <>
                  <span className="h-3.5 w-px shrink-0 bg-[var(--border)]" aria-hidden="true" />
                  <EffortPill
                    embedded
                    effort={thinkingEffort}
                    ultracodeEnabled={ultracodeEnabled}
                    open={activePopover === "effort"}
                    disabled={runActive}
                    onClick={() =>
                      setActivePopover((p) => (p === "effort" ? null : "effort"))
                    }
                  />
                </>
              )}
            </div>

            {runActive && (
              <button
                type="button"
                onClick={onToggleSubmitMode}
                title={t.a11y.switchGuidanceMode}
                className="inline-flex h-[28px] shrink-0 items-center gap-1 whitespace-nowrap rounded-[var(--radius-md)] border border-[var(--border)] bg-[var(--surface-raised)] px-2 text-[11px] text-[var(--text-secondary)] transition-colors hover:border-[var(--border-strong)]"
              >
                <span>{submitMode === "queue" ? t.composer.queue : t.composer.steer}</span>
                <ChevronDown className="h-3 w-3 text-[var(--text-tertiary)]" strokeWidth={1.75} />
              </button>
            )}

            {compactionPending && !queuedPromptPreview && (
              <Pill
                tone="info"
                className="h-[28px] shrink-0 whitespace-nowrap"
              >
                <Minimize2 className="h-3 w-3" strokeWidth={1.75} />
                <span>{t.composer.compactionQueued}</span>
              </Pill>
            )}

            {queuedPromptPreview && (
              <button
                type="button"
                onClick={onClearQueuedPrompt}
                title={`${t.composer.queuedPrompt}: ${queuedPromptPreview}`}
                className="inline-flex h-[28px] max-w-[140px] shrink-0 items-center gap-1 whitespace-nowrap rounded-[var(--radius-md)] border border-[var(--warning)] bg-[var(--warning-surface)] px-2 text-[11px] text-[var(--warning)] transition-colors hover:opacity-90"
              >
                <span className="truncate">{t.composer.queuedPrompt}</span>
                <X className="h-3 w-3 shrink-0" strokeWidth={1.75} />
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

            <Button
              variant={isStopState || canSend ? "primary" : "secondary"}
              size="icon"
              onClick={() => void handleActionClick()}
              disabled={isStopState ? cancelling : !canSend}
              aria-label={
                isStopState ? t.a11y.cancelRun : t.a11y.sendPrompt
              }
              title={
                isStopState ? t.a11y.cancelRun : t.a11y.sendPrompt
              }
              className="ml-0.5 h-[28px] w-[28px] shrink-0 rounded-full"
            >
              {isStopState ? (
                <Square className="h-2.5 w-2.5 fill-current" />
              ) : (
                <ArrowUp className="h-3.5 w-3.5" strokeWidth={2.2} />
              )}
            </Button>
          </div>
        </div>
      </div>
    </div>
  );

  if (variant === "home") {
    return <div className="w-full max-w-[620px]">{cardInner}</div>;
  }

  return (
    <div className="bg-[var(--surface-app)] px-6 pb-3.5 pt-2">
      <div className="mx-auto flex max-w-[840px] flex-col gap-2">
        {cardInner}
        <div className="flex items-center justify-center gap-4 text-[11px] text-[var(--text-tertiary)]">
          <span>{t.composer.hintEnter}</span>
          <span>{t.composer.hintShiftEnter}</span>
          <span>{t.composer.hintTab}</span>
          <span>{t.composer.hintEsc}</span>
        </div>
      </div>
    </div>
  );
}
