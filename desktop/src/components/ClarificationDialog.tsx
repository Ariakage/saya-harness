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
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4 backdrop-blur-xs"
    >
      <div className="w-full max-w-lg rounded-xl border border-[#58A6FF]/50 bg-[#141416] shadow-2xl">
        <div className="flex items-center gap-2 border-b border-[#303038] px-4 py-3">
          <MessageSquareMore className="h-4 w-4 text-[#58A6FF]" />
          <h2 className="text-sm font-semibold text-[#E8E8ED]">
            Clarification Needed
          </h2>
        </div>

        <div className="space-y-3 p-4 text-xs">
          <p className="text-sm leading-relaxed text-[#E8E8ED]">
            {sanitizeDisplayText(request.question)}
          </p>

          {request.options.length > 0 && (
            <div className="space-y-1.5">
              <div className="text-[10px] uppercase tracking-wider text-[#81818B]">
                Suggested Options
              </div>
              <div className="flex flex-col gap-1.5">
                {request.options.map((opt, idx) => (
                  <button
                    key={opt}
                    type="button"
                    disabled={submitting}
                    onClick={() => void submitAnswer(opt)}
                    className="flex items-center gap-2 rounded-md border border-[#303038] bg-[#09090B] px-3 py-2 text-left text-xs text-[#E8E8ED] hover:border-[#58A6FF] hover:bg-[#202024] disabled:opacity-50"
                  >
                    <span className="rounded bg-[#202024] px-1.5 py-0.5 font-mono text-[10px] text-[#58A6FF]">
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
              className="block text-[10px] uppercase tracking-wider text-[#81818B]"
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
                className="flex-1 rounded-md border border-[#303038] bg-[#202024] px-3 py-1.5 text-xs text-[#E8E8ED] placeholder:text-[#81818B] focus:border-[#58A6FF] focus:outline-none"
              />
              <button
                type="button"
                disabled={submitting || customAnswer.trim().length === 0}
                onClick={() => void submitAnswer(customAnswer)}
                className="rounded-md bg-[#58A6FF] px-3.5 py-1.5 font-semibold text-[#09090B] hover:bg-[#73B5FF] disabled:opacity-40"
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
