import { formatTokenCount } from "../lib/sanitize";

export interface ContextRingProps {
  /** Tokens currently occupying the context window, when known. */
  usedTokens: number | null;
  /** Total context window capacity for the active profile. */
  windowTokens: number | null;
  /** Whether `usedTokens` is an estimate rather than a provider report. */
  estimated?: boolean;
  /** Pixel size of the square indicator. */
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
  const color = arcColor(percent);
  const tooltip = `${estimated ? "~" : ""}${formatTokenCount(used)} / ${formatTokenCount(windowTokens)} (${percent}%)`;

  return (
    <span
      role="img"
      aria-label={`Context usage ${percent}%`}
      title={`Context: ${tooltip}`}
      className="relative inline-flex shrink-0 items-center justify-center rounded-full transition-[background] duration-300 ease-out"
      style={{
        width: size,
        height: size,
        // Conic-gradient arc drawn as a ring via a radial mask: the colored
        // arc covers `percent` of the circle, the rest stays the track color.
        background: `conic-gradient(${color} ${percent}%, #3a3a3a ${percent}% 100%)`,
        WebkitMask:
          "radial-gradient(farthest-side, transparent calc(100% - 3px), #000 calc(100% - 2.6px))",
        mask: "radial-gradient(farthest-side, transparent calc(100% - 3px), #000 calc(100% - 2.6px))",
      }}
    />
  );
}
