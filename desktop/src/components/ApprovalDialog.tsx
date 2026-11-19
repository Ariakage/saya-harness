import { AlertTriangle } from "lucide-react";
import { useEffect, useState } from "react";
import type { ApprovalRequestDto_Deserialize } from "../bindings";
import { getMessages } from "../lib/i18n";
import { WORK_MODE_STYLES, sanitizeDisplayText } from "../lib/sanitize";
import { useAppStore } from "../store/useAppStore";
import { Button } from "./ui/Button";

export interface ApprovalDialogProps {
  request: ApprovalRequestDto_Deserialize;
  onReply: (approved: boolean) => Promise<boolean>;
}

export function ApprovalDialog({ request, onReply }: ApprovalDialogProps) {
  const locale = useAppStore((s) => s.locale);
  const t = getMessages(locale);
  const [submitting, setSubmitting] = useState(false);

  const handleDecision = async (approved: boolean) => {
    if (submitting) return;
    setSubmitting(true);
    try {
      await onReply(approved);
    } finally {
      setSubmitting(false);
    }
  };

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (submitting) return;
      if (e.key === "y" || e.key === "Y") {
        e.preventDefault();
        void handleDecision(true);
      } else if (e.key === "n" || e.key === "N") {
        e.preventDefault();
        void handleDecision(false);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  });

  const modeStyle = WORK_MODE_STYLES[request.work_mode];

  return (
    <section
      role="region"
      aria-label={t.a11y.approvalRegion}
      data-testid="approval-dialog"
      className="soyo-msg-enter rounded-[12px] border border-[var(--warning)] bg-[var(--warning-surface)] p-[14px] text-xs shadow-[var(--shadow-sm)]"
    >
      <div className="flex items-center justify-between gap-3">
        <div className="flex items-center gap-2 text-[13px] font-semibold text-[var(--text-primary)]">
          <AlertTriangle className="h-3.5 w-3.5 shrink-0 text-[var(--warning)]" />
          <span>{t.approval.title}</span>
        </div>
        <span className={`soyo-pill ${modeStyle.badgeClass}`}>
          {modeStyle.label}
        </span>
      </div>

      <div className="my-2.5 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 rounded-[9px] border border-[var(--border)] bg-[var(--surface-raised)] px-3 py-2.5 text-[12px]">
        <span className="text-[var(--text-tertiary)]">{t.approval.tool}</span>
        <span className="font-mono text-[11.5px] text-[var(--text-primary)]">
          {sanitizeDisplayText(request.tool)}
        </span>
        {request.action && (
          <>
            <span className="text-[var(--text-tertiary)]">{t.approval.action}</span>
            <span className="font-mono text-[11.5px] text-[var(--info)]">
              {sanitizeDisplayText(request.action)}
            </span>
          </>
        )}
        <span className="text-[var(--text-tertiary)]">{t.approval.call}</span>
        <span className="font-mono text-[11.5px] text-[var(--text-primary)]">
          {sanitizeDisplayText(request.call_id)}
        </span>
        <span className="text-[var(--text-tertiary)]">{t.approval.workspace}</span>
        <span className="truncate font-mono text-[11.5px] text-[var(--text-primary)]">
          {sanitizeDisplayText(request.cwd)}
        </span>
      </div>

      <div className="text-[12.5px] text-[var(--text-secondary)]">
        {sanitizeDisplayText(request.operation_summary)}
      </div>

      <pre className="my-2 max-h-48 overflow-auto whitespace-pre-wrap rounded-[8px] border border-[var(--border)] bg-[var(--surface-code)] px-2.5 py-2 font-mono text-[12px] text-[var(--text-primary)]">
        {sanitizeDisplayText(request.formatted_call)}
      </pre>

      <div className="mt-2.5 flex flex-wrap items-center justify-between gap-2">
        <span className="text-[11px] text-[var(--text-tertiary)]">
          {t.approval.pressPrefix} <strong className="text-[var(--text-secondary)]">Y</strong>{" "}
          {t.approval.pressMid} <strong className="text-[var(--text-secondary)]">N</strong>{" "}
          {t.approval.pressSuffix}
        </span>
        <div className="flex items-center gap-2">
          <Button
            variant="secondary"
            size="sm"
            disabled={submitting}
            onClick={() => void handleDecision(false)}
          >
            {t.approval.denyBtn}
          </Button>
          <Button
            variant="primary"
            size="sm"
            disabled={submitting}
            onClick={() => void handleDecision(true)}
          >
            {t.approval.allowBtn}
          </Button>
        </div>
      </div>
    </section>
  );
}
