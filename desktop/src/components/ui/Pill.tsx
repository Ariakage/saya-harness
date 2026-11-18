import type { ButtonHTMLAttributes, HTMLAttributes, ReactNode } from "react";

export type PillTone =
  | "neutral"
  | "accent"
  | "info"
  | "success"
  | "warning"
  | "danger"
  | "ultra";

const TONE_CLASSES: Record<PillTone, string> = {
  neutral:
    "border-[var(--border)] bg-[var(--surface-raised)] text-[var(--text-secondary)]",
  accent:
    "border-[var(--accent)] bg-[var(--accent-surface)] text-[var(--accent-text)]",
  info: "border-[var(--border-strong)] bg-[var(--info-surface)] text-[var(--info)]",
  success:
    "border-[var(--border-strong)] bg-[var(--success-surface)] text-[var(--success)]",
  warning:
    "border-[var(--border-strong)] bg-[var(--warning-surface)] text-[var(--warning)]",
  danger:
    "border-[var(--danger)] bg-[var(--danger-surface)] text-[var(--danger)]",
  ultra:
    "border-[var(--ultra)] bg-[var(--ultra-surface)] text-[var(--ultra-bright)]",
};

interface StaticPillProps extends HTMLAttributes<HTMLSpanElement> {
  tone?: PillTone;
  children: ReactNode;
}

export function Pill({
  tone = "neutral",
  className = "",
  children,
  ...rest
}: StaticPillProps) {
  return (
    <span
      className={`inline-flex shrink-0 items-center gap-1.5 rounded-[var(--radius-md)] border px-2 py-0.5 text-[12px] leading-[18px] font-medium whitespace-nowrap transition-colors duration-150 ${TONE_CLASSES[tone]} ${className}`.trim()}
      {...rest}
    >
      {children}
    </span>
  );
}

interface PillButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  tone?: PillTone;
  children: ReactNode;
}

export function PillButton({
  tone = "neutral",
  type = "button",
  className = "",
  children,
  ...rest
}: PillButtonProps) {
  return (
    <button
      type={type}
      className={`inline-flex shrink-0 items-center gap-1.5 rounded-[var(--radius-md)] border px-2.5 py-1 text-[12px] leading-[18px] font-medium whitespace-nowrap transition-colors duration-150 hover:bg-[var(--surface-hover)] disabled:cursor-not-allowed disabled:opacity-40 ${TONE_CLASSES[tone]} ${className}`.trim()}
      {...rest}
    >
      {children}
    </button>
  );
}
