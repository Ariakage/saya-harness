import { ArrowRight, Hand, Shield, ShieldAlert } from "lucide-react";
import type { PermissionModeDto, WorkModeDto } from "../bindings";
import { getMessages } from "../lib/i18n";
import { PERMISSION_MODE_LABELS, WORK_MODE_STYLES } from "../lib/sanitize";
import { useAppStore } from "../store/useAppStore";

export interface ModeAndPermissionBarProps {
  workMode: WorkModeDto;
  pendingMode: WorkModeDto | null;
  permissionMode: PermissionModeDto;
  runActive: boolean;
  onSelectMode: (mode: WorkModeDto) => Promise<boolean>;
  onSelectPermission: (permissions: PermissionModeDto) => Promise<boolean>;
}

const MODES: WorkModeDto[] = ["work", "code", "plan", "build"];
const PERMISSIONS: PermissionModeDto[] = ["auto", "bypass", "manual"];

export function ModeAndPermissionBar({
  workMode,
  pendingMode,
  permissionMode,
  runActive,
  onSelectMode,
  onSelectPermission,
}: ModeAndPermissionBarProps) {
  const locale = useAppStore((s) => s.locale);
  const t = getMessages(locale);
  const isBypass = permissionMode === "bypass";
  const permissionTone = isBypass
    ? "text-[var(--danger)]"
    : permissionMode === "manual"
      ? "text-[var(--info)]"
      : "text-[var(--text-secondary)]";

  return (
    <div className="flex flex-wrap items-center gap-1.5" data-testid="mode-permission-bar">
      <div
        role="group"
        aria-label={t.a11y.workMode}
        className="flex items-center gap-0.5 rounded-[var(--radius-md)] border border-[var(--border)] bg-[var(--surface-raised)] p-0.5"
      >
        {MODES.map((mode) => {
          const style = WORK_MODE_STYLES[mode];
          const isCurrent = workMode === mode;
          const isPending = pendingMode === mode && pendingMode !== workMode;
          return (
            <button
              key={mode}
              type="button"
              onClick={() => void onSelectMode(mode)}
              aria-pressed={isCurrent}
              className={`rounded-[var(--radius-sm)] px-2 py-0.5 text-[11px] leading-[16px] font-medium transition-colors ${
                isCurrent
                  ? "bg-[var(--surface-hover)] font-semibold"
                  : isPending
                    ? "border border-dashed border-[var(--border-strong)] text-[var(--text-primary)]"
                    : "text-[var(--text-tertiary)] hover:text-[var(--text-primary)]"
              }`}
              style={isCurrent ? { color: style.color } : undefined}
              title={
                isPending
                  ? t.a11y.queuedModeTip(style.label)
                  : t.a11y.switchModeTip(style.label)
              }
            >
              {style.label}
              {isPending ? "*" : ""}
            </button>
          );
        })}
      </div>

      {pendingMode && pendingMode !== workMode && (
        <span className="soyo-pill text-[var(--text-tertiary)]">
          <ArrowRight className="h-3 w-3" strokeWidth={1.75} aria-hidden="true" />
          {WORK_MODE_STYLES[pendingMode].label}
        </span>
      )}

      <div
        className={`soyo-pill ${isBypass ? "border-[var(--danger)] bg-[var(--danger-surface)]" : ""}`}
      >
        {isBypass ? (
          <ShieldAlert
            className="h-3 w-3 text-[var(--danger)]"
            strokeWidth={1.75}
            aria-hidden="true"
          />
        ) : permissionMode === "manual" ? (
          <Hand
            className="h-3 w-3 text-[var(--info)]"
            strokeWidth={1.75}
            aria-hidden="true"
          />
        ) : (
          <Shield
            className="h-3 w-3 text-[var(--text-secondary)]"
            strokeWidth={1.75}
            aria-hidden="true"
          />
        )}
        <select
          aria-label={t.a11y.permissionMode}
          value={permissionMode}
          disabled={runActive}
          onChange={(e) =>
            void onSelectPermission(e.target.value as PermissionModeDto)
          }
          className={`soyo-select is-inline text-[12px] font-medium ${permissionTone}`}
        >
          {PERMISSIONS.map((perm) => (
            <option
              key={perm}
              value={perm}
              className="bg-[var(--surface-overlay)] text-[var(--text-primary)]"
            >
              {PERMISSION_MODE_LABELS[perm]}
            </option>
          ))}
        </select>
      </div>
    </div>
  );
}
