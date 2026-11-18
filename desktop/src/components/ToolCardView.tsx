import { Bot, ChevronDown, ChevronRight, Terminal } from "lucide-react";
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

  const toolNameLower = (message.toolName ?? message.label).toLowerCase();
  const isSubagent =
    toolNameLower.includes("subagent") ||
    toolNameLower.includes("delegate") ||
    toolNameLower.includes("agent_");

  const statusPill = (() => {
    switch (message.toolState) {
      case "requested":
        return <span className="devin-pill text-[#7aa2f7]">requested</span>;
      case "running":
        return <span className="devin-pill text-[#fac454]">running</span>;
      case "error":
        return <span className="devin-pill text-[#f87171]">error</span>;
      case "ok":
        return <span className="devin-pill text-[#4ade80]">done</span>;
      default:
        return message.toolOk === true ? (
          <span className="devin-pill text-[#4ade80]">done</span>
        ) : message.toolOk === false ? (
          <span className="devin-pill text-[#f87171]">error</span>
        ) : (
          <span className="devin-pill text-[#a3a3a3]">tool</span>
        );
    }
  })();

  if (isSubagent) {
    return (
      <div
        data-testid="tool-card"
        className="rounded-[10px] border border-[#2c2c2c] border-l-2 border-l-[#d9a08e] bg-[#171717] px-3 py-2.5 text-[12.5px]"
      >
        <button
          type="button"
          onClick={() => setExpanded((prev) => !prev)}
          className="flex w-full items-center gap-2 text-left"
        >
          <Bot className="h-3.5 w-3.5 shrink-0 text-[#d9a08e]" />
          <span className="font-medium text-[#d9d9d9]">
            {message.toolName ?? "Subagent"}
          </span>
          <span className="devin-pill text-[#7aa2f7]">subagent</span>
          {statusPill}
          <span className="flex-1" />
          {message.toolSummary && (
            <span className="max-w-xs truncate text-[11.5px] text-[#6e6e6e]">
              {message.toolSummary}
            </span>
          )}
          {expanded ? (
            <ChevronDown className="h-3.5 w-3.5 shrink-0 text-[#6e6e6e]" />
          ) : (
            <ChevronRight className="h-3.5 w-3.5 shrink-0 text-[#6e6e6e]" />
          )}
        </button>

        {expanded && (
          <div className="ml-[15px] mt-2 space-y-2 border-l border-[#2c2c2c] pl-3 text-xs text-[#8a8a8a]">
            {message.toolRequestFormatted && (
              <pre className="max-h-40 overflow-auto whitespace-pre-wrap rounded-[8px] border border-[#262626] bg-[#121212] p-2 font-mono text-[11px] text-[#c9c9c9]">
                {message.toolRequestFormatted}
              </pre>
            )}
            {message.toolError && (
              <div className="text-[11px] text-[#f87171]">
                {message.toolError.code}: {message.toolError.message}
              </div>
            )}
            {message.text && (
              <pre className="max-h-56 overflow-auto whitespace-pre-wrap font-mono text-[11.5px] text-[#c9c9c9]">
                {message.text}
              </pre>
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

  return (
    <div className="text-[12.5px]" data-testid="tool-card">
      <button
        type="button"
        onClick={() => setExpanded((prev) => !prev)}
        className="flex w-full items-center gap-2.5 px-0.5 py-[6px] text-left text-[#9a9a9a] transition-colors hover:text-[#d9d9d9]"
      >
        <Terminal className="h-3.5 w-3.5 shrink-0 text-[#8a8a8a]" />
        <span className="truncate font-mono text-[#e8e8e8]">
          {message.toolName ?? message.label}
        </span>
        {statusPill}
        {message.truncated && (
          <span className="devin-pill text-[#fac454]">truncated</span>
        )}
        <span className="flex-1" />
        {message.toolSummary && (
          <span className="max-w-sm truncate font-mono text-[11.5px] text-[#6e6e6e]">
            {message.toolSummary}
          </span>
        )}
        {expanded ? (
          <ChevronDown className="h-3.5 w-3.5 shrink-0 text-[#6e6e6e]" />
        ) : (
          <ChevronRight className="h-3.5 w-3.5 shrink-0 text-[#6e6e6e]" />
        )}
      </button>

      {expanded && (
        <div className="ml-[7px] mt-1 space-y-2 border-l-2 border-[#2c2c2c] pb-1.5 pl-3.5 text-xs">
          {message.toolReviewNote && (
            <div className="rounded-[8px] border border-[#262626] bg-[#1a1a1a] px-2.5 py-1.5 text-[11px] text-[#a3a3a3]">
              Review: {message.toolReviewNote}
            </div>
          )}
          {message.toolRequestFormatted &&
            message.toolRequestFormatted !== message.text && (
              <div>
                <div className="mb-1 text-[10px] uppercase tracking-wider text-[#6e6e6e]">
                  Request
                </div>
                <pre className="max-h-40 overflow-auto whitespace-pre-wrap rounded-[8px] border border-[#262626] bg-[#161616] p-2 font-mono text-[11px] text-[#c9d1d9]">
                  {message.toolRequestFormatted}
                </pre>
              </div>
            )}
          {message.toolError && (
            <div className="rounded-[8px] border border-[#f87171]/35 bg-[#1c1515] px-2.5 py-1.5 text-[11px] text-[#f87171]">
              {message.toolError.code}: {message.toolError.message}
            </div>
          )}
          {message.text && (
            <div>
              <div className="mb-1 text-[10px] uppercase tracking-wider text-[#6e6e6e]">
                Output
              </div>
              <pre className="max-h-64 overflow-auto whitespace-pre-wrap rounded-[8px] border border-[#262626] bg-[#161616] p-2 font-mono text-[11px] text-[#c9d1d9]">
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
