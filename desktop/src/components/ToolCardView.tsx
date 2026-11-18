import { CheckCircle2, ChevronDown, ChevronRight, Clock, Terminal, XCircle } from "lucide-react";
import { useState } from "react";
import type { DesktopMessage } from "../store/useAppStore";
import { SessionMediaPreview } from "./AttachmentPreview";

export interface ToolCardViewProps {
  message: DesktopMessage;
  sessionId?: string | null;
}

export function ToolCardView({ message, sessionId = null }: ToolCardViewProps) {
  const [expanded, setExpanded] = useState(
    message.toolOk === false || message.mediaRefs.length > 0
  );

  const statusBadge = (() => {
    switch (message.toolState) {
      case "requested":
        return (
          <span className="inline-flex items-center gap-1 rounded bg-[#58A6FF]/15 px-1.5 py-0.5 text-[10px] text-[#58A6FF]">
            <Clock className="h-3 w-3" />
            requested
          </span>
        );
      case "running":
        return (
          <span className="inline-flex items-center gap-1 rounded bg-[#F6AD55]/15 px-1.5 py-0.5 text-[10px] text-[#F6AD55]">
            <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-[#F6AD55]" />
            running
          </span>
        );
      case "error":
        return (
          <span className="inline-flex items-center gap-1 rounded bg-[#F87171]/15 px-1.5 py-0.5 text-[10px] text-[#F87171]">
            <XCircle className="h-3 w-3" />
            failed
          </span>
        );
      case "ok":
      default:
        return (
          <span className="inline-flex items-center gap-1 rounded bg-[#7DCB9B]/15 px-1.5 py-0.5 text-[10px] text-[#7DCB9B]">
            <CheckCircle2 className="h-3 w-3" />
            done
          </span>
        );
    }
  })();

  return (
    <div
      className="rounded-lg border border-[#303038] bg-[#141416] text-xs"
      data-testid="tool-card"
    >
      <button
        type="button"
        onClick={() => setExpanded((prev) => !prev)}
        className="flex w-full items-center justify-between gap-2 px-3 py-2 text-left hover:bg-[#202024]/50"
      >
        <div className="flex min-w-0 items-center gap-2">
          <Terminal className="h-3.5 w-3.5 shrink-0 text-[#58A6FF]" />
          <span className="truncate font-mono font-medium text-[#E8E8ED]">
            {message.toolName ?? message.label}
          </span>
          {statusBadge}
          {message.truncated && (
            <span className="rounded border border-[#F6AD55]/40 bg-[#F6AD55]/10 px-1.5 py-0.5 text-[10px] text-[#F6AD55]">
              truncated
            </span>
          )}
        </div>
        <div className="flex min-w-0 items-center gap-2 text-[#81818B]">
          {message.toolSummary && (
            <span className="truncate text-[11px]">{message.toolSummary}</span>
          )}
          {expanded ? (
            <ChevronDown className="h-3.5 w-3.5 shrink-0" />
          ) : (
            <ChevronRight className="h-3.5 w-3.5 shrink-0" />
          )}
        </div>
      </button>

      {expanded && (
        <div className="space-y-2 border-t border-[#303038] px-3 py-2.5">
          {message.toolReviewNote && (
            <div className="rounded border border-[#303038] bg-[#09090B] px-2 py-1 text-[11px] text-[#81818B]">
              Review: {message.toolReviewNote}
            </div>
          )}
          {message.toolRequestFormatted &&
            message.toolRequestFormatted !== message.text && (
              <div>
                <div className="mb-1 text-[10px] uppercase tracking-wider text-[#81818B]">
                  Request
                </div>
                <pre className="max-h-40 overflow-auto whitespace-pre-wrap rounded bg-[#09090B] p-2 font-mono text-[11px] text-[#D4D4D8]">
                  {message.toolRequestFormatted}
                </pre>
              </div>
            )}
          {message.toolError && (
            <div className="rounded border border-[#F87171]/40 bg-[#F87171]/10 px-2.5 py-1.5 text-[11px] text-[#F87171]">
              {message.toolError.code}: {message.toolError.message}
            </div>
          )}
          {message.text && (
            <div>
              <div className="mb-1 text-[10px] uppercase tracking-wider text-[#81818B]">
                Output
              </div>
              <pre className="max-h-64 overflow-auto whitespace-pre-wrap rounded bg-[#09090B] p-2 font-mono text-[11px] text-[#D4D4D8]">
                {message.text}
              </pre>
            </div>
          )}
          <SessionMediaPreview
            sessionId={sessionId}
            mediaRefs={message.mediaRefs}
          />
        </div>
      )}
    </div>
  );
}
