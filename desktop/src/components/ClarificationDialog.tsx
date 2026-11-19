import { MessageSquareMore } from "lucide-react";
import { useState } from "react";
import type { InputRequestDto } from "../bindings";
import { getMessages } from "../lib/i18n";
import { sanitizeDisplayText } from "../lib/sanitize";
import { useAppStore } from "../store/useAppStore";
import { Button } from "./ui/Button";
import { DialogShell } from "./ui/DialogShell";

export interface ClarificationDialogProps {
  request: InputRequestDto;
  onReply: (answer: string) => Promise<boolean>;
}

export function ClarificationDialog({
  request,
  onReply,
}: ClarificationDialogProps) {
  const locale = useAppStore((s) => s.locale);
  const t = getMessages(locale);
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
    <DialogShell
      open
      ariaLabel={t.a11y.clarificationDialog}
      testId="clarification-dialog"
      size="sm"
      panelClassName="max-w-lg"
    >
      <div className="flex items-center gap-2 border-b border-[var(--border)] px-4 py-3">
        <MessageSquareMore className="h-4 w-4 text-[var(--accent-text)]" />
        <h2 className="text-[14px] font-semibold text-[var(--text-primary)]">
          {t.clarification.title}
        </h2>
      </div>

      <div className="space-y-3 p-4 text-xs">
        <p className="text-sm leading-relaxed text-[var(--text-primary)]">
          {sanitizeDisplayText(request.question)}
        </p>

        {request.options.length > 0 && (
          <div className="space-y-1.5">
            <div className="text-[11px] uppercase tracking-wider text-[var(--text-tertiary)]">
              {t.clarification.suggestedOptions}
            </div>
            <div className="flex flex-col gap-1.5">
              {request.options.map((opt, idx) => (
                <button
                  key={opt}
                  type="button"
                  disabled={submitting}
                  onClick={() => void submitAnswer(opt)}
                  className="flex items-center gap-2 rounded-[9px] border border-[var(--border)] bg-[var(--surface-raised)] px-3 py-2 text-left text-xs text-[var(--text-primary)] transition-colors hover:border-[var(--accent)] hover:bg-[var(--surface-hover)] disabled:opacity-50"
                >
                  <span className="soyo-pill py-0 font-mono text-[11px] text-[var(--info)]">
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
            className="block text-[11px] uppercase tracking-wider text-[var(--text-tertiary)]"
          >
            {t.clarification.customAnswer}
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
              placeholder={t.clarification.placeholder}
              className="soyo-input flex-1 px-3 py-1.5 text-xs"
            />
            <Button
              variant="primary"
              size="md"
              disabled={submitting || customAnswer.trim().length === 0}
              onClick={() => void submitAnswer(customAnswer)}
            >
              {t.clarification.submitBtn}
            </Button>
          </div>
        </div>
      </div>
    </DialogShell>
  );
}
