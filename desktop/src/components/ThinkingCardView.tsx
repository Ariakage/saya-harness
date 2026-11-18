import { Brain, ChevronDown, ChevronRight } from "lucide-react";
import { useState } from "react";
import type { ThinkingSnapshotDto } from "../bindings";

export interface ThinkingCardViewProps {
  entries: ThinkingSnapshotDto[];
}

export function ThinkingCardView({ entries }: ThinkingCardViewProps) {
  const [expanded, setExpanded] = useState(false);

  if (entries.length === 0) {
    return null;
  }

  const latest = entries[entries.length - 1];
  const anyActive = entries.some((entry) => entry.active);
  const anyTruncated = entries.some((entry) => entry.truncated);

  return (
    <div
      className="rounded-lg border border-[#303038] bg-[#141416] text-xs"
      data-testid="thinking-card"
    >
      <button
        type="button"
        onClick={() => setExpanded((prev) => !prev)}
        className="flex w-full items-center justify-between gap-2 px-3 py-2 text-left hover:bg-[#202024]/50"
      >
        <div className="flex items-center gap-2">
          <Brain className="h-3.5 w-3.5 text-[#BB9AF7]" />
          <span className="font-medium text-[#E8E8ED]">Thinking</span>
          {anyActive && (
            <span className="inline-flex items-center gap-1 rounded bg-[#BB9AF7]/15 px-1.5 py-0.5 text-[10px] text-[#BB9AF7]">
              <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-[#BB9AF7]" />
              reasoning
            </span>
          )}
          {anyTruncated && (
            <span className="rounded border border-[#303038] px-1.5 py-0.5 text-[10px] text-[#81818B]">
              truncated
            </span>
          )}
        </div>
        <div className="flex items-center gap-2 text-[#81818B]">
          {!expanded && latest.available && (
            <span className="max-w-md truncate text-[11px]">
              {latest.text.trim().split("\n").pop() || "Thinking..."}
            </span>
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
          {entries.map((entry) => (
            <div key={entry.request_id} className="space-y-1">
              <div className="flex items-center justify-between text-[10px] text-[#81818B]">
                <span className="font-mono">{entry.request_id}</span>
                <span>
                  {entry.active ? "active" : entry.available ? "done" : "redacted"}
                </span>
              </div>
              {entry.available ? (
                <pre className="max-h-60 overflow-y-auto whitespace-pre-wrap rounded bg-[#09090B] p-2 font-mono text-[11px] leading-relaxed text-[#D4D4D8]">
                  {entry.text || "(empty)"}
                </pre>
              ) : (
                <div className="rounded bg-[#09090B] px-2 py-1.5 text-[11px] text-[#81818B]">
                  Reasoning trace unavailable or redacted.
                </div>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
