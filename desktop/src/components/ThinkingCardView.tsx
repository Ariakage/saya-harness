import { ChevronDown, ChevronRight } from "lucide-react";
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
    <div className="text-[12.5px]" data-testid="thinking-card">
      <button
        type="button"
        onClick={() => setExpanded((prev) => !prev)}
        className="flex w-full items-center gap-2 px-0.5 py-1.5 text-left text-[#8a8a8a] transition-colors hover:text-[#c9c9c9]"
      >
        {expanded ? (
          <ChevronDown className="h-3.5 w-3.5 shrink-0 text-[#6e6e6e]" />
        ) : (
          <ChevronRight className="h-3.5 w-3.5 shrink-0 text-[#6e6e6e]" />
        )}
        <span>{expanded ? "Thinking" : anyActive ? "Thinking…" : "Thought"}</span>
        {!expanded && (
          <span className="devin-pill text-[#a3a3a3]">
            {anyActive ? "reasoning" : "auto-collapsed"}
          </span>
        )}
        {anyTruncated && (
          <span className="devin-pill text-[#7a7a7a]">truncated</span>
        )}
        {!expanded && latest.available && (
          <span className="ml-auto max-w-md truncate text-[11.5px] text-[#6e6e6e]">
            {latest.text.trim().split("\n").pop() || ""}
          </span>
        )}
      </button>

      {expanded && (
        <div className="ml-[7px] mt-0.5 space-y-2 border-l-2 border-[#2c2c2c] py-0.5 pl-[14px] text-[12.5px] leading-[1.65] text-[#8a8a8a]">
          {entries.map((entry) => (
            <div key={entry.request_id} className="space-y-1">
              {entry.available ? (
                <div className="whitespace-pre-wrap">
                  {entry.text || "(empty)"}
                </div>
              ) : (
                <div className="text-[#6e6e6e]">
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
