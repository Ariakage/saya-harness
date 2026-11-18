import { Minimize2, Paperclip, Plus, Send, Square } from "lucide-react";
import { type KeyboardEvent, useState } from "react";
import type { StagedAttachmentDto } from "../bindings";
import { StagedAttachmentsStrip } from "./AttachmentPreview";

export interface ComposerProps {
  disabled: boolean;
  runActive: boolean;
  cancelling: boolean;
  hasSession: boolean;
  stagedAttachments?: StagedAttachmentDto[];
  onSubmit: (prompt: string) => Promise<boolean>;
  onCancel: () => Promise<void>;
  onCompact: () => Promise<boolean>;
  onCycleMode?: () => Promise<boolean>;
  onStageAttachments?: (paths: string[]) => Promise<boolean>;
  onRemoveAttachment?: (id: string) => void;
}

export function Composer({
  disabled,
  runActive,
  cancelling,
  hasSession,
  stagedAttachments = [],
  onSubmit,
  onCancel,
  onCompact,
  onCycleMode,
  onStageAttachments,
  onRemoveAttachment,
}: ComposerProps) {
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const [attachOpen, setAttachOpen] = useState(false);
  const [attachPath, setAttachPath] = useState("");
  const [staging, setStaging] = useState(false);

  const handleStage = async () => {
    const trimmed = attachPath.trim();
    if (!trimmed || !onStageAttachments || staging || disabled) {
      return;
    }
    setStaging(true);
    try {
      const ok = await onStageAttachments([trimmed]);
      if (ok) {
        setAttachPath("");
        setAttachOpen(false);
      }
    } finally {
      setStaging(false);
    }
  };

  const handleSend = async () => {
    const trimmed = draft.trim();
    if ((!trimmed && stagedAttachments.length === 0) || disabled || sending) {
      return;
    }
    setSending(true);
    try {
      const ok = await onSubmit(trimmed);
      if (ok) {
        setDraft("");
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
    if (event.key === "Escape" && runActive) {
      event.preventDefault();
      void onCancel();
      return;
    }
    if (
      event.key === "Enter" &&
      !event.shiftKey &&
      !event.altKey &&
      !event.nativeEvent.isComposing
    ) {
      event.preventDefault();
      void handleSend();
    }
  };

  return (
    <div className="border-t border-[#303038] bg-[#141416] px-4 py-3">
      <div className="mx-auto flex max-w-4xl flex-col gap-2">
        <StagedAttachmentsStrip
          items={stagedAttachments}
          onRemove={(id) => onRemoveAttachment?.(id)}
        />
        {attachOpen && (
          <div className="flex items-center gap-2 rounded-md border border-[#303038] bg-[#09090B] p-2 text-xs">
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
              className="flex-1 rounded border border-[#303038] bg-[#202024] px-2.5 py-1 font-mono text-xs text-[#E8E8ED] focus:border-[#58A6FF] focus:outline-none"
            />
            <button
              type="button"
              disabled={disabled || staging || attachPath.trim().length === 0}
              onClick={() => void handleStage()}
              className="inline-flex items-center gap-1 rounded bg-[#58A6FF] px-2.5 py-1 text-[11px] font-semibold text-[#09090B] disabled:opacity-40"
            >
              <Plus className="h-3 w-3" />
              Stage
            </button>
          </div>
        )}
        <div className="flex items-end gap-2 rounded-lg border border-[#303038] bg-[#202024] p-2 focus-within:border-[#58A6FF]">
          <textarea
            aria-label="Prompt input"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={handleKeyDown}
            disabled={disabled}
            rows={2}
            placeholder={
              runActive
                ? "Run active — type guidance to steer, or press Esc / Cancel..."
                : "Ask Soyo to analyze, edit, or run commands (Enter to send, Shift+Enter for newline)..."
            }
            className="max-h-40 min-h-[44px] flex-1 resize-y bg-transparent px-1.5 py-1 text-sm text-[#E8E8ED] placeholder:text-[#81818B] focus:outline-none disabled:opacity-50"
          />
          <div className="flex shrink-0 items-center gap-1.5">
            {onStageAttachments && (
              <button
                type="button"
                onClick={() => setAttachOpen((prev) => !prev)}
                disabled={disabled || runActive}
                title="Stage local attachment"
                className="inline-flex items-center gap-1 rounded-md border border-[#303038] bg-[#141416] px-2.5 py-1.5 text-xs text-[#D4D4D8] hover:border-[#58A6FF] disabled:opacity-40"
              >
                <Paperclip className="h-3.5 w-3.5" />
                Attach
              </button>
            )}
            {hasSession && !runActive && (
              <button
                type="button"
                onClick={() => void onCompact()}
                disabled={disabled}
                title="Compact session history"
                className="inline-flex items-center gap-1 rounded-md border border-[#303038] bg-[#141416] px-2.5 py-1.5 text-xs text-[#D4D4D8] hover:border-[#58A6FF] disabled:opacity-40"
              >
                <Minimize2 className="h-3.5 w-3.5" />
                Compact
              </button>
            )}
            {runActive && (
              <button
                type="button"
                onClick={() => void onCancel()}
                disabled={cancelling}
                className="inline-flex items-center gap-1 rounded-md border border-[#F87171]/50 bg-[#F87171]/15 px-2.5 py-1.5 text-xs font-medium text-[#F87171] hover:bg-[#F87171]/25 disabled:opacity-50"
              >
                <Square className="h-3 w-3 fill-current" />
                {cancelling ? "Cancelling..." : "Cancel"}
              </button>
            )}
            <button
              type="button"
              onClick={() => void handleSend()}
              disabled={
                disabled ||
                sending ||
                (draft.trim().length === 0 && stagedAttachments.length === 0)
              }
              className="inline-flex items-center gap-1 rounded-md bg-[#58A6FF] px-3 py-1.5 text-xs font-semibold text-[#09090B] hover:bg-[#73B5FF] disabled:cursor-not-allowed disabled:opacity-40"
            >
              <Send className="h-3.5 w-3.5" />
              {runActive ? "Steer" : "Send"}
            </button>
          </div>
        </div>
        <div className="flex items-center justify-between px-1 text-[11px] text-[#81818B]">
          <span>
            Enter send · Shift+Enter newline · Empty Tab cycle mode · Esc cancel
          </span>
          <span>Schema v1 in-process runtime</span>
        </div>
      </div>
    </div>
  );
}
