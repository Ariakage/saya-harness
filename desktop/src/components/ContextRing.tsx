import { formatTokenCount } from "../lib/sanitize";

export interface ContextRingProps {
  /** Tokens currently occupying the context window, when known. */
  usedTokens: number | null;
  /** Total context window capacity for the active profile. */
  windowTokens: number | null;
  /** Whether `usedTokens` is an estimate rather than a provider report. */
  estimated?: boolean;
  /** Pixel size of the square SVG. */
  size?: number;
}

function arcColor(percent: number): string {
  if (percent >= 90) return "#f87171";
  if (percent >= 70) return "#fac454";
  return "#d9a08e";
}

/** Circular context-window fill indicator rendered in the composer toolbar. */
export function ContextRing({
  usedTokens,
  windowTokens,
  estimated = false,
  size = 20,
}: ContextRingProps) {
  if (!windowTokens || windowTokens <= 0) {
    return null;
  }
  const used = Math.max(0, usedTokens ?? 0);
  const percent = Math.min(100, Math.round((used / windowTokens) * 100));
  const strokeWidth = 2.4;
  const radius = (size - strokeWidth) / 2;
  const center = size / 2;
  const circumference = 2 * Math.PI * radius;
  const dashOffset = circumference * (1 - percent / 100);
  const color = arcColor(percent);
  const tooltip = `${estimated ? "~" : ""}${formatTokenCount(used)} / ${formatTokenCount(windowTokens)} (${percent}%)`;

  return (
    <span
      role="img"
      aria-label={`Context usage ${percent}%`}
      title={`Context: ${tooltip}`}
      className="inline-flex shrink-0 items-center justify-center"
    >
      <svg
        width={size}
        height={size}
        viewBox={`0 0 ${size} ${size}`}
        className="-rotate-90"
      >
        <circle
          cx={center}
          cy={center}
          r={radius}
          fill="none"
          stroke="#3a3a3a"
          strokeWidth={strokeWidth}
        />
        <circle
          cx={center}
          cy={center}
          r={radius}
          fill="none"
          stroke={color}
          strokeWidth={strokeWidth}
          strokeLinecap="round"
          strokeDasharray={circumference}
          strokeDashoffset={dashOffset}
          className="transition-[stroke-dashoffset,stroke] duration-300 ease-out"
        />
      </svg>
    </span>
  );
}
