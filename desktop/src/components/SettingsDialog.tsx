import { Settings, X } from "lucide-react";
import { useEffect, useState } from "react";
import {
  commands,
  type BootstrapStateDto_Serialize,
  type SandboxBackendDto,
} from "../bindings";
import { ModulesPanel } from "./ModulesPanel";

export interface SettingsDialogProps {
  open: boolean;
  bootstrap: BootstrapStateDto_Serialize | null;
  runActive: boolean;
  onClose: () => void;
  onBootstrapUpdated: (boot: BootstrapStateDto_Serialize) => void;
}

const EFFORTS = ["default", "low", "medium", "high", "xhigh", "max"] as const;
const BACKENDS: SandboxBackendDto[] = ["auto", "seatbelt", "bubblewrap", "host"];

export function SettingsDialog({
  open,
  bootstrap,
  runActive,
  onClose,
  onBootstrapUpdated,
}: SettingsDialogProps) {
  const [workspaceInput, setWorkspaceInput] = useState("");
  const [triggerInput, setTriggerInput] = useState("");
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (bootstrap) {
      setWorkspaceInput(bootstrap.workspace);
      setTriggerInput(
        bootstrap.auto_compact_trigger_tokens != null
          ? String(bootstrap.auto_compact_trigger_tokens)
          : ""
      );
    }
  }, [bootstrap, open]);

  if (!open) {
    return null;
  }

  const runUpdate = async (
    action: () => Promise<
      | { status: "ok"; data: BootstrapStateDto_Serialize }
      | { status: "error"; error: { code: string; message: string } }
    >
  ) => {
    setErrorMsg(null);
    setSaving(true);
    try {
      const res = await action();
      if (res.status === "ok") {
        onBootstrapUpdated(res.data);
      } else {
        setErrorMsg(`${res.error.code}: ${res.error.message}`);
      }
    } finally {
      setSaving(false);
    }
  };

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Desktop Settings"
      data-testid="settings-dialog"
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4 backdrop-blur-xs"
    >
      <div className="flex max-h-[85vh] w-full max-w-2xl flex-col rounded-xl border border-[#303038] bg-[#141416] shadow-2xl">
        <div className="flex items-center justify-between border-b border-[#303038] px-4 py-3">
          <div className="flex items-center gap-2">
            <Settings className="h-4 w-4 text-[#58A6FF]" />
            <h2 className="text-sm font-semibold text-[#E8E8ED]">
              Runtime & Module Settings
            </h2>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close settings"
            className="rounded p-1 text-[#81818B] hover:bg-[#202024] hover:text-[#E8E8ED]"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="flex-1 space-y-5 overflow-y-auto p-4 text-xs">
          {errorMsg && (
            <div className="rounded border border-[#F87171]/40 bg-[#F87171]/10 px-3 py-2 text-[#F87171]">
              {errorMsg}
            </div>
          )}

          <div className="space-y-2">
            <label
              htmlFor="settings-workspace-input"
              className="block font-medium text-[#E8E8ED]"
            >
              Workspace Directory
            </label>
            <div className="flex gap-2">
              <input
                id="settings-workspace-input"
                type="text"
                value={workspaceInput}
                disabled={runActive || saving}
                onChange={(e) => setWorkspaceInput(e.target.value)}
                className="flex-1 rounded-md border border-[#303038] bg-[#202024] px-2.5 py-1.5 font-mono text-xs text-[#E8E8ED] focus:border-[#58A6FF] focus:outline-none disabled:opacity-50"
              />
              <button
                type="button"
                disabled={runActive || saving || !workspaceInput.trim()}
                onClick={() =>
                  void runUpdate(() => commands.setWorkspace(workspaceInput))
                }
                className="rounded-md border border-[#303038] bg-[#202024] px-3 py-1.5 font-medium text-[#E8E8ED] hover:border-[#58A6FF] disabled:opacity-40"
              >
                Apply
              </button>
            </div>
          </div>

          <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
            <div className="space-y-1.5">
              <label
                htmlFor="settings-profile-select"
                className="block font-medium text-[#E8E8ED]"
              >
                Model Profile
              </label>
              <select
                id="settings-profile-select"
                value={bootstrap?.selected_profile ?? ""}
                disabled={runActive || saving}
                onChange={(e) =>
                  void runUpdate(() => commands.selectProfile(e.target.value))
                }
                className="w-full rounded-md border border-[#303038] bg-[#202024] px-2 py-1.5 text-xs text-[#E8E8ED] disabled:opacity-50"
              >
                {(bootstrap?.profiles ?? []).map((p) => (
                  <option key={p.name} value={p.name}>
                    {p.name} ({p.model})
                  </option>
                ))}
              </select>
            </div>

            <div className="space-y-1.5">
              <label
                htmlFor="settings-effort-select"
                className="block font-medium text-[#E8E8ED]"
              >
                Thinking Effort
              </label>
              <select
                id="settings-effort-select"
                value={bootstrap?.thinking_effort ?? "default"}
                disabled={runActive || saving}
                onChange={(e) => {
                  const val = e.target.value;
                  void runUpdate(() =>
                    commands.setThinkingEffort(val === "default" ? null : val)
                  );
                }}
                className="w-full rounded-md border border-[#303038] bg-[#202024] px-2 py-1.5 text-xs text-[#E8E8ED] disabled:opacity-50"
              >
                {EFFORTS.map((eff) => (
                  <option key={eff} value={eff}>
                    {eff}
                  </option>
                ))}
              </select>
            </div>

            <div className="space-y-1.5">
              <label
                htmlFor="settings-sandbox-select"
                className="block font-medium text-[#E8E8ED]"
              >
                Sandbox Backend
              </label>
              <select
                id="settings-sandbox-select"
                value={bootstrap?.sandbox_backend ?? "auto"}
                disabled={runActive || saving}
                onChange={(e) =>
                  void runUpdate(() =>
                    commands.setSandboxBackend(e.target.value as SandboxBackendDto)
                  )
                }
                className="w-full rounded-md border border-[#303038] bg-[#202024] px-2 py-1.5 text-xs text-[#E8E8ED] disabled:opacity-50"
              >
                {BACKENDS.map((bk) => (
                  <option key={bk} value={bk}>
                    {bk}
                  </option>
                ))}
              </select>
            </div>
          </div>

          <div className="space-y-2 rounded-md border border-[#303038] bg-[#09090B] p-3">
            <div className="flex items-center justify-between">
              <span className="font-medium text-[#E8E8ED]">
                Auto-Compact ({bootstrap?.auto_compact_trigger_percent ?? 80}%)
              </span>
              <button
                type="button"
                disabled={runActive || saving}
                onClick={() =>
                  void runUpdate(() =>
                    commands.setAutocompact(!bootstrap?.auto_compact, null)
                  )
                }
                className={`rounded px-2.5 py-1 text-[11px] font-semibold ${
                  bootstrap?.auto_compact
                    ? "border border-[#7DCB9B]/40 bg-[#7DCB9B]/20 text-[#7DCB9B]"
                    : "border border-[#303038] bg-[#202024] text-[#81818B]"
                } disabled:opacity-40`}
              >
                {bootstrap?.auto_compact ? "On" : "Off"}
              </button>
            </div>
            <div className="flex items-center gap-2">
              <input
                type="number"
                aria-label="Auto-compact trigger tokens"
                value={triggerInput}
                disabled={runActive || saving}
                onChange={(e) => setTriggerInput(e.target.value)}
                placeholder="Optional exact token trigger (blank = % threshold)"
                className="flex-1 rounded-md border border-[#303038] bg-[#202024] px-2.5 py-1 text-xs text-[#E8E8ED] placeholder:text-[#81818B]"
              />
              <button
                type="button"
                disabled={runActive || saving}
                onClick={() => {
                  const parsed = triggerInput.trim()
                    ? Number(triggerInput.trim())
                    : null;
                  void runUpdate(() => commands.setAutocompact(true, parsed));
                }}
                className="rounded-md border border-[#303038] bg-[#202024] px-2.5 py-1 text-[#E8E8ED] hover:border-[#58A6FF] disabled:opacity-40"
              >
                Set Trigger
              </button>
            </div>
          </div>

          <div className="border-t border-[#303038] pt-4">
            <ModulesPanel runActive={runActive} />
          </div>
        </div>
      </div>
    </div>
  );
}
