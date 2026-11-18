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
    color: "#d9a08e",
    badgeClass: "border-[#2e2e2e] bg-[#1c1c1c] text-[#d9a08e]",
  },
  code: {
    label: "CODE",
    color: "#4ade80",
    badgeClass: "border-[#2e2e2e] bg-[#1c1c1c] text-[#4ade80]",
  },
  plan: {
    label: "PLAN",
    color: "#a096eb",
    badgeClass: "border-[#2e2e2e] bg-[#1c1c1c] text-[#a096eb]",
  },
  build: {
    label: "BUILD",
    color: "#fac454",
    badgeClass: "border-[#2e2e2e] bg-[#1c1c1c] text-[#fac454]",
  },
};

export const PERMISSION_MODE_LABELS: Record<PermissionModeDto, string> = {
  auto: "auto",
  bypass: "bypass permissions",
  manual: "Manual Approval",
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
