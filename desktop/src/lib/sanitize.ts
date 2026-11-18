import type { PermissionModeDto, WorkModeDto } from "../bindings";

// Strip ANSI CSI/OSC sequences and C0/DEL control characters except \n and \t.
const ANSI_SEQUENCE_RE =
  // eslint-disable-next-line no-control-regex
  /\u001b(?:\[[0-9;?]*[ -/]*[@-~]|\][^\u0007\u001b]*(?:\u0007|\u001b\\)|[@-Z\\-_])/g;
// eslint-disable-next-line no-control-regex
const CONTROL_CHAR_RE = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/g;

export function sanitizeDisplayText(input: string): string {
  return input.replace(ANSI_SEQUENCE_RE, "").replace(CONTROL_CHAR_RE, "");
}

export type WorkModeVisual = {
  label: string;
  color: string;
  badgeClass: string;
};

export const WORK_MODE_STYLES: Record<WorkModeDto, WorkModeVisual> = {
  work: {
    label: "WORK",
    color: "var(--mode-work)",
    badgeClass:
      "border-[var(--border)] bg-[var(--surface-raised)] text-[var(--mode-work)]",
  },
  code: {
    label: "CODE",
    color: "var(--mode-code)",
    badgeClass:
      "border-[var(--border)] bg-[var(--surface-raised)] text-[var(--mode-code)]",
  },
  plan: {
    label: "PLAN",
    color: "var(--mode-plan)",
    badgeClass:
      "border-[var(--border)] bg-[var(--surface-raised)] text-[var(--mode-plan)]",
  },
  build: {
    label: "BUILD",
    color: "var(--mode-build)",
    badgeClass:
      "border-[var(--border)] bg-[var(--surface-raised)] text-[var(--mode-build)]",
  },
};

export const PERMISSION_MODE_LABELS: Record<PermissionModeDto, string> = {
  auto: "Auto",
  bypass: "Bypass permissions",
  manual: "Manual approval",
};

export function formatTokenCount(tokens: number | null | undefined): string {
  if (tokens == null || Number.isNaN(tokens)) {
    return "0";
  }
  if (tokens >= 1_000_000_000) {
    return `${(tokens / 1_000_000_000).toFixed(1)}B`;
  }
  if (tokens >= 1_000_000) {
    return `${(tokens / 1_000_000).toFixed(1)}M`;
  }
  if (tokens >= 1000) {
    return `${(tokens / 1000).toFixed(1)}k`;
  }
  return String(tokens);
}
