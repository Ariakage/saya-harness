import { ShieldAlert } from "lucide-react";
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
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Approval Required"
      data-testid="approval-dialog"
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4 backdrop-blur-xs"
    >
      <div className="w-full max-w-xl rounded-xl border border-[#F6AD55]/50 bg-[#141416] shadow-2xl">
        <div className="flex items-center justify-between border-b border-[#303038] px-4 py-3">
          <div className="flex items-center gap-2">
            <ShieldAlert className="h-4 w-4 text-[#F6AD55]" />
            <h2 className="text-sm font-semibold text-[#E8E8ED]">
              Approval Required
            </h2>
          </div>
          <span
            className={`rounded border px-1.5 py-0.5 text-[10px] font-semibold ${modeStyle.badgeClass}`}
          >
            {modeStyle.label}
          </span>
        </div>

        <div className="space-y-3 p-4 text-xs">
          <div className="grid grid-cols-2 gap-2 rounded-md border border-[#303038] bg-[#09090B] p-2.5 text-[11px]">
            <div>
              <span className="text-[#81818B]">Tool: </span>
              <span className="font-mono text-[#E8E8ED]">
                {sanitizeDisplayText(request.tool)}
              </span>
            </div>
            <div>
              <span className="text-[#81818B]">Call: </span>
              <span className="font-mono text-[#D4D4D8]">
                {sanitizeDisplayText(request.call_id)}
              </span>
            </div>
            {request.action && (
              <div className="col-span-2">
                <span className="text-[#81818B]">Action: </span>
                <span className="font-mono text-[#F6AD55]">
                  {sanitizeDisplayText(request.action)}
                </span>
              </div>
            )}
            <div className="col-span-2 truncate">
              <span className="text-[#81818B]">Workspace: </span>
              <span className="font-mono text-[#D4D4D8]">
                {sanitizeDisplayText(request.cwd)}
              </span>
            </div>
          </div>

          <div className="text-[#E8E8ED]">
            {sanitizeDisplayText(request.operation_summary)}
          </div>

          <div>
            <div className="mb-1 text-[10px] uppercase tracking-wider text-[#81818B]">
              Formatted Call
            </div>
            <pre className="max-h-48 overflow-auto whitespace-pre-wrap rounded-md border border-[#303038] bg-[#09090B] p-2.5 font-mono text-[11px] text-[#D4D4D8]">
              {sanitizeDisplayText(request.formatted_call)}
            </pre>
          </div>
        </div>

        <div className="flex items-center justify-between border-t border-[#303038] px-4 py-3 text-xs">
          <span className="text-[11px] text-[#81818B]">
            Press <kbd className="rounded bg-[#202024] px-1">Y</kbd> to allow or{" "}
            <kbd className="rounded bg-[#202024] px-1">N</kbd> to deny
          </span>
          <div className="flex items-center gap-2">
            <button
              type="button"
              disabled={submitting}
              onClick={() => void handleDecision(false)}
              className="rounded-md border border-[#F87171]/50 bg-[#F87171]/10 px-3 py-1.5 font-medium text-[#F87171] hover:bg-[#F87171]/20 disabled:opacity-50"
            >
              Deny (N)
            </button>
            <button
              type="button"
              disabled={submitting}
              onClick={() => void handleDecision(true)}
              className="rounded-md bg-[#7DCB9B] px-3.5 py-1.5 font-semibold text-[#09090B] hover:bg-[#8ED8AB] disabled:opacity-50"
            >
              Allow (Y)
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
