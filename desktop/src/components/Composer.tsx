import { Minimize2, Send, Square } from "lucide-react";
import { type KeyboardEvent, useState } from "react";

export interface ComposerProps {
  disabled: boolean;
  runActive: boolean;
  cancelling: boolean;
  hasSession: boolean;
  onSubmit: (prompt: string) => Promise<boolean>;
  onCancel: () => Promise<void>;
  onCompact: () => Promise<boolean>;
  onCycleMode?: () => Promise<boolean>;
}

export function Composer({
  disabled,
  runActive,
  cancelling,
  hasSession,
  onSubmit,
  onCancel,
  onCompact,
  onCycleMode,
}: ComposerProps) {
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);

  const handleSend = async () => {
    const trimmed = draft.trim();
    if (!trimmed || disabled || sending) {
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
              disabled={disabled || sending || draft.trim().length === 0}
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
