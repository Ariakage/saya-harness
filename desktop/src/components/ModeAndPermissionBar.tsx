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
    <div className="flex flex-wrap items-center gap-2" data-testid="mode-permission-bar">
      <div
        role="group"
        aria-label="Work mode"
        className="flex items-center rounded-md border border-[#303038] bg-[#09090B] p-0.5"
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
              className={`rounded px-2 py-0.5 text-[10px] font-semibold transition-colors ${
                isCurrent
                  ? style.badgeClass
                  : isPending
                    ? "border border-dashed border-[#81818B] text-[#E8E8ED]"
                    : "text-[#81818B] hover:text-[#E8E8ED]"
              }`}
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
        <span className="rounded border border-[#303038] bg-[#202024] px-1.5 py-0.5 text-[10px] text-[#81818B]">
          → {WORK_MODE_STYLES[pendingMode].label} next
        </span>
      )}

      <div className="inline-flex items-center gap-1 rounded-md border border-[#303038] bg-[#09090B] px-2 py-0.5 text-[11px] text-[#D4D4D8]">
        <Shield className="h-3 w-3 text-[#81818B]" />
        <select
          aria-label="Permission mode"
          value={permissionMode}
          disabled={runActive}
          onChange={(e) =>
            void onSelectPermission(e.target.value as PermissionModeDto)
          }
          className="bg-transparent text-[11px] text-[#D4D4D8] focus:outline-none disabled:opacity-50"
        >
          {PERMISSIONS.map((perm) => (
            <option key={perm} value={perm} className="bg-[#141416]">
              {PERMISSION_MODE_LABELS[perm]}
            </option>
          ))}
        </select>
      </div>
    </div>
  );
}
