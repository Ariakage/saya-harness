import { AlertTriangle } from "lucide-react";
import { useEffect, useState } from "react";
import type { ApprovalRequestDto_Deserialize } from "../bindings";
import { WORK_MODE_STYLES, sanitizeDisplayText } from "../lib/sanitize";

export interface ApprovalDialogProps {
  request: ApprovalRequestDto_Deserialize;
  onReply: (approved: boolean) => Promise<boolean>;
}

export function ApprovalDialog({ request, onReply }: ApprovalDialogProps) {
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
      aria-label="Approval Required"
      data-testid="approval-dialog"
      className="rounded-[12px] border border-[#4a3f2a] bg-[#1c1a15] p-[14px] text-xs"
    >
      <div className="flex items-center justify-between gap-3">
        <div className="flex items-center gap-2 text-[13px] font-semibold text-[#e8d9b0]">
          <AlertTriangle className="h-3.5 w-3.5 shrink-0 text-[#fac454]" />
          <span>Approval required</span>
        </div>
        <span className={`devin-pill ${modeStyle.badgeClass}`}>
          {modeStyle.label}
        </span>
      </div>

      <div className="my-2.5 grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 rounded-[9px] border border-[#2e2820] bg-[#171512] px-3 py-2.5 text-[12px]">
        <span className="text-[#8a8a8a]">Tool</span>
        <span className="font-mono text-[11.5px] text-[#d9d9d9]">
          {sanitizeDisplayText(request.tool)}
        </span>
        {request.action && (
          <>
            <span className="text-[#8a8a8a]">Action</span>
            <span className="font-mono text-[11.5px] text-[#7aa2f7]">
              {sanitizeDisplayText(request.action)}
            </span>
          </>
        )}
        <span className="text-[#8a8a8a]">Call</span>
        <span className="font-mono text-[11.5px] text-[#d9d9d9]">
          {sanitizeDisplayText(request.call_id)}
        </span>
        <span className="text-[#8a8a8a]">Workspace</span>
        <span className="truncate font-mono text-[11.5px] text-[#d9d9d9]">
          {sanitizeDisplayText(request.cwd)}
        </span>
      </div>

      <div className="text-[12.5px] text-[#c9c9c9]">
        {sanitizeDisplayText(request.operation_summary)}
      </div>

      <pre className="my-2 max-h-48 overflow-auto whitespace-pre-wrap rounded-[8px] border border-[#2e2820] bg-[#121110] px-2.5 py-2 font-mono text-[12px] text-[#e8e8e8]">
        {sanitizeDisplayText(request.formatted_call)}
      </pre>

      <div className="mt-2.5 flex flex-wrap items-center justify-between gap-2">
        <span className="text-[11px] text-[#6e6e6e]">
          Press <strong className="text-[#a3a3a3]">Y</strong> to allow or{" "}
          <strong className="text-[#a3a3a3]">N</strong> to deny
        </span>
        <div className="flex items-center gap-2">
          <button
            type="button"
            disabled={submitting}
            onClick={() => void handleDecision(false)}
            className="devin-pill-btn text-[#f87171] disabled:opacity-40"
          >
            Deny (N)
          </button>
          <button
            type="button"
            disabled={submitting}
            onClick={() => void handleDecision(true)}
            className="devin-pill-btn text-[#4ade80] disabled:opacity-40"
          >
            Allow (Y)
          </button>
        </div>
      </div>
    </section>
  );
}
