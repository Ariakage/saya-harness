import { MessageSquareMore } from "lucide-react";
import { useState } from "react";
import type { InputRequestDto } from "../bindings";
import { sanitizeDisplayText } from "../lib/sanitize";

export interface ClarificationDialogProps {
  request: InputRequestDto;
  onReply: (answer: string) => Promise<boolean>;
}

export function ClarificationDialog({
  request,
  onReply,
}: ClarificationDialogProps) {
  const [customAnswer, setCustomAnswer] = useState("");
  const [submitting, setSubmitting] = useState(false);

  const submitAnswer = async (value: string) => {
    const trimmed = value.trim();
    if (!trimmed || submitting) return;
    setSubmitting(true);
    try {
      await onReply(trimmed);
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Clarification Needed"
      data-testid="clarification-dialog"
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4 backdrop-blur-xs"
    >
      <div className="w-full max-w-lg overflow-hidden rounded-[14px] border border-[#2b2b2b] bg-[#181818] shadow-[0_24px_60px_rgba(0,0,0,0.65)]">
        <div className="flex items-center gap-2 border-b border-[#232323] px-4 py-3">
          <MessageSquareMore className="h-4 w-4 text-[#d9a08e]" />
          <h2 className="text-[14px] font-semibold text-[#e8e8e8]">
            Clarification Needed
          </h2>
        </div>

        <div className="space-y-3 p-4 text-xs">
          <p className="text-sm leading-relaxed text-[#e8e8e8]">
            {sanitizeDisplayText(request.question)}
          </p>

          {request.options.length > 0 && (
            <div className="space-y-1.5">
              <div className="text-[10px] uppercase tracking-wider text-[#6e6e6e]">
                Suggested Options
              </div>
              <div className="flex flex-col gap-1.5">
                {request.options.map((opt, idx) => (
                  <button
                    key={opt}
                    type="button"
                    disabled={submitting}
                    onClick={() => void submitAnswer(opt)}
                    className="flex items-center gap-2 rounded-[9px] border border-[#262626] bg-[#1a1a1a] px-3 py-2 text-left text-xs text-[#e8e8e8] transition-colors hover:border-[#d9a08e]/50 hover:bg-[#222222] disabled:opacity-50"
                  >
                    <span className="devin-pill py-0 font-mono text-[10px] text-[#7aa2f7]">
                      {idx + 1}
                    </span>
                    <span>{sanitizeDisplayText(opt)}</span>
                  </button>
                ))}
              </div>
            </div>
          )}

          <div className="space-y-1.5">
            <label
              htmlFor="clarification-custom-input"
              className="block text-[10px] uppercase tracking-wider text-[#6e6e6e]"
            >
              Custom Answer
            </label>
            <div className="flex gap-2">
              <input
                id="clarification-custom-input"
                type="text"
                value={customAnswer}
                onChange={(e) => setCustomAnswer(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    void submitAnswer(customAnswer);
                  }
                }}
                placeholder="Type your response..."
                className="flex-1 rounded-[8px] border border-[#2b2b2b] bg-[#1e1e1e] px-3 py-1.5 text-xs text-[#e8e8e8] placeholder:text-[#6e6e6e] focus:border-[#d9a08e] focus:outline-none"
              />
              <button
                type="button"
                disabled={submitting || customAnswer.trim().length === 0}
                onClick={() => void submitAnswer(customAnswer)}
                className="rounded-[8px] bg-[#d9a08e] px-3.5 py-1.5 font-semibold text-[#1c120e] hover:bg-[#e2a68f] disabled:opacity-40"
              >
                Submit
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
