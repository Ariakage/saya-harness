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
    color: "#58A6FF",
    badgeClass: "border-[#58A6FF]/40 bg-[#58A6FF]/15 text-[#58A6FF]",
  },
  code: {
    label: "CODE",
    color: "#7DCB9B",
    badgeClass: "border-[#7DCB9B]/40 bg-[#7DCB9B]/15 text-[#7DCB9B]",
  },
  plan: {
    label: "PLAN",
    color: "#BB9AF7",
    badgeClass: "border-[#BB9AF7]/40 bg-[#BB9AF7]/15 text-[#BB9AF7]",
  },
  build: {
    label: "BUILD",
    color: "#F6AD55",
    badgeClass: "border-[#F6AD55]/40 bg-[#F6AD55]/15 text-[#F6AD55]",
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
  if (tokens >= 1000) {
    return `${(tokens / 1000).toFixed(1)}k`;
  }
  return String(tokens);
}
