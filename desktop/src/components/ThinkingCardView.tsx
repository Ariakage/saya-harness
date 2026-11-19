import { ChevronRight } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { ThinkingSnapshotDto } from "../bindings";
import { getMessages } from "../lib/i18n";
import { useAppStore } from "../store/useAppStore";
import { SquaresLoader } from "./SquaresLoader";

function formatThinkingSeconds(ms: number): string {
  const seconds = Math.max(1, Math.round(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}

export interface ThinkingCardViewProps {
  entries: ThinkingSnapshotDto[];
}

export function ThinkingCardView({ entries }: ThinkingCardViewProps) {
  const locale = useAppStore((s) => s.locale);
  const t = getMessages(locale);
  const [expanded, setExpanded] = useState(false);
  const [durationMs, setDurationMs] = useState<number | null>(null);
  const startedAtRef = useRef<number | null>(null);
  const anyActive = entries.some((entry) => entry.active);

  // Track wall-clock thinking time from first active snapshot to settle.
  useEffect(() => {
    if (anyActive) {
      if (startedAtRef.current === null) {
        startedAtRef.current = Date.now();
      }
      return;
    }
    if (startedAtRef.current !== null) {
      setDurationMs(Date.now() - startedAtRef.current);
      startedAtRef.current = null;
    }
  }, [anyActive]);

  if (entries.length === 0) {
    return null;
  }

  const latest = entries[entries.length - 1];
  const anyTruncated = entries.some((entry) => entry.truncated);

  return (
    <div className="soyo-msg-enter text-[13px]" data-testid="thinking-card">
      <button
        type="button"
        onClick={() => setExpanded((prev) => !prev)}
        aria-expanded={expanded}
        className="group flex w-full items-center gap-2 rounded-[var(--radius-md)] px-1 py-1.5 text-left text-[var(--text-tertiary)] transition-colors hover:bg-[var(--surface-raised)] hover:text-[var(--text-secondary)]"
      >
        <ChevronRight
          className={`h-3.5 w-3.5 shrink-0 text-[var(--text-tertiary)] transition-transform duration-200 ease-out ${
            expanded ? "rotate-90" : ""
          }`}
        />
        <span>{expanded ? t.cards.thinking : anyActive ? t.cards.thinkingActive : t.cards.thought}</span>
        {!expanded && (
          <span className="soyo-pill text-[var(--text-secondary)]">
            {anyActive && <SquaresLoader size={12} />}
            {anyActive ? t.cards.reasoning : t.cards.autoCollapsed}
          </span>
        )}
        {!anyActive && durationMs !== null && (
          <span className="soyo-pill text-[var(--text-secondary)]" data-testid="thinking-duration">
            {t.extra.thinkingDuration(formatThinkingSeconds(durationMs))}
          </span>
        )}
        {anyTruncated && (
          <span className="soyo-pill text-[var(--text-tertiary)]">{t.cards.truncated}</span>
        )}
        {!expanded && latest.available && (
          <span className="ml-auto max-w-md truncate text-[12px] text-[var(--text-tertiary)]">
            {latest.text.trim().split("\n").pop() || ""}
          </span>
        )}
      </button>

      {expanded && (
        <div className="soyo-panel-enter ml-[7px] mt-0.5 space-y-2 border-l-2 border-[var(--border)] py-0.5 pl-[14px] text-[13px] leading-[20px] text-[var(--text-tertiary)]">
          {entries.map((entry) => (
            <div key={entry.request_id} className="space-y-1">
              {entry.available ? (
                <div className="whitespace-pre-wrap">
                  {entry.text || t.cards.empty}
                </div>
              ) : (
                <div className="text-[var(--text-tertiary)]">
                  {t.cards.reasoningUnavailable}
                </div>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
