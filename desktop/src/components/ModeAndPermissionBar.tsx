import { Shield } from "lucide-react";
import type { PermissionModeDto, WorkModeDto } from "../bindings";
import { PERMISSION_MODE_LABELS, WORK_MODE_STYLES } from "../lib/sanitize";

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
  return (
    <div className="flex flex-wrap items-center gap-1.5" data-testid="mode-permission-bar">
      <div
        role="group"
        aria-label="Work mode"
        className="flex items-center gap-0.5 rounded-[8px] border border-[#2b2b2b] bg-[#1a1a1a] p-0.5"
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
              className={`rounded-[6px] px-2 py-0.5 text-[11px] font-medium transition-colors ${
                isCurrent
                  ? "bg-[#262626] font-semibold"
                  : isPending
                    ? "border border-dashed border-[#6e6e6e] text-[#e8e8e8]"
                    : "text-[#8a8a8a] hover:text-[#e8e8e8]"
              }`}
              style={isCurrent ? { color: style.color } : undefined}
              title={
                isPending
                  ? `Queued ${style.label} for next turn`
                  : `Switch to ${style.label} mode`
              }
            >
              {style.label}
              {isPending ? "*" : ""}
            </button>
          );
        })}
      </div>

      {pendingMode && pendingMode !== workMode && (
        <span className="devin-pill text-[#8a8a8a]">
          → {WORK_MODE_STYLES[pendingMode].label} next
        </span>
      )}

      <div className="devin-pill">
        <Shield className="h-3 w-3 text-[#7a7a7a]" />
        <select
          aria-label="Permission mode"
          value={permissionMode}
          disabled={runActive}
          onChange={(e) =>
            void onSelectPermission(e.target.value as PermissionModeDto)
          }
          className={`bg-transparent text-[12px] font-medium focus:outline-none disabled:opacity-50 ${
            permissionMode === "manual" ? "text-[#fac454]" : "text-[#8aa8d8]"
          }`}
        >
          {PERMISSIONS.map((perm) => (
            <option key={perm} value={perm} className="bg-[#181818] text-[#e8e8e8]">
              {PERMISSION_MODE_LABELS[perm]}
            </option>
          ))}
        </select>
      </div>
    </div>
  );
}
