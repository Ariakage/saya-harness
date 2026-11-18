import { AlertTriangle, Blocks, Briefcase, Puzzle, XCircle } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import {
  commands,
  type BackgroundJobDto_Serialize,
  type ExtensionItemDto_Serialize,
  type ModuleStatusDto_Serialize,
} from "../bindings";
import { sanitizeDisplayText } from "../lib/sanitize";

export interface ModulesPanelProps {
  runActive: boolean;
}

export function ModulesPanel({ runActive }: ModulesPanelProps) {
  const [modules, setModules] = useState<ModuleStatusDto_Serialize[]>([]);
  const [extensions, setExtensions] = useState<ExtensionItemDto_Serialize[]>([]);
  const [jobs, setJobs] = useState<BackgroundJobDto_Serialize[]>([]);
  const [statusMessage, setStatusMessage] = useState<string | null>(null);

  const loadAll = useCallback(async () => {
    const [modRes, extRes, jobRes] = await Promise.all([
      commands.listModules(),
      commands.listExtensions(),
      commands.listJobs(),
    ]);
    if (modRes.status === "ok") setModules(modRes.data);
    if (extRes.status === "ok") setExtensions(extRes.data);
    if (jobRes.status === "ok") setJobs(jobRes.data);
  }, []);

  useEffect(() => {
    void loadAll();
  }, [loadAll]);

  const handleToggleModule = async (mod: ModuleStatusDto_Serialize) => {
    setStatusMessage(null);
    const res = await commands.setModuleEnabled(mod.module, !mod.enabled);
    if (res.status === "ok") {
      setModules(res.data);
    } else {
      setStatusMessage(`${res.error.code}: ${res.error.message}`);
    }
  };

  const handleCancelJob = async (jobId: string) => {
    setStatusMessage(null);
    const res = await commands.cancelBackgroundJob(jobId);
    if (res.status === "ok") {
      await loadAll();
    } else {
      setStatusMessage(`${res.error.code}: ${res.error.message}`);
    }
  };

  return (
    <div className="space-y-4 text-xs" data-testid="modules-panel">
      {statusMessage && (
        <div className="rounded border border-[#F87171]/40 bg-[#F87171]/10 px-2.5 py-1.5 text-[11px] text-[#F87171]">
          {statusMessage}
        </div>
      )}

      <section className="space-y-2">
        <div className="flex items-center gap-1.5 font-medium text-[#E8E8ED]">
          <Blocks className="h-3.5 w-3.5 text-[#58A6FF]" />
          <span>Runtime Modules</span>
        </div>
        {modules.length === 0 ? (
          <div className="text-[11px] text-[#81818B]">No module status available.</div>
        ) : (
          <ul className="space-y-2">
            {modules.map((mod) => (
              <li
                key={mod.module}
                className="flex items-start justify-between gap-3 rounded-md border border-[#303038] bg-[#09090B] p-2.5"
              >
                <div className="min-w-0 space-y-1">
                  <div className="flex items-center gap-2">
                    <span className="font-mono font-semibold text-[#E8E8ED]">
                      {mod.module}
                    </span>
                    <span className="rounded border border-[#303038] bg-[#141416] px-1.5 py-0.5 text-[10px] text-[#81818B]">
                      {mod.source}
                    </span>
                  </div>
                  <div className="text-[11px] text-[#81818B]">
                    {sanitizeDisplayText(mod.description)}
                  </div>
                  {mod.issue && (
                    <div className="flex items-center gap-1 text-[11px] text-[#F6AD55]">
                      <AlertTriangle className="h-3 w-3 shrink-0" />
                      <span>{sanitizeDisplayText(mod.issue)}</span>
                    </div>
                  )}
                </div>
                <button
                  type="button"
                  disabled={runActive}
                  onClick={() => void handleToggleModule(mod)}
                  className={`shrink-0 rounded-md px-2.5 py-1 text-[11px] font-semibold ${
                    mod.enabled
                      ? "bg-[#7DCB9B]/20 text-[#7DCB9B] border border-[#7DCB9B]/40"
                      : "bg-[#202024] text-[#81818B] border border-[#303038]"
                  } disabled:opacity-40`}
                >
                  {mod.enabled ? "Enabled" : "Disabled"}
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="space-y-2">
        <div className="flex items-center gap-1.5 font-medium text-[#E8E8ED]">
          <Puzzle className="h-3.5 w-3.5 text-[#BB9AF7]" />
          <span>Extensions & Project Discovery ({extensions.length})</span>
        </div>
        {extensions.length === 0 ? (
          <div className="text-[11px] text-[#81818B]">
            No discovered rules, skills, or plugins in current workspace.
          </div>
        ) : (
          <ul className="max-h-40 space-y-1 overflow-y-auto">
            {extensions.map((ext) => (
              <li
                key={`${ext.kind}:${ext.id}`}
                className="flex items-center justify-between gap-2 rounded border border-[#303038] bg-[#09090B] px-2.5 py-1.5 text-[11px]"
              >
                <div className="min-w-0">
                  <span className="mr-1.5 rounded bg-[#202024] px-1 py-0.5 font-mono text-[10px] text-[#BB9AF7]">
                    {ext.kind}
                  </span>
                  <span className="font-medium text-[#E8E8ED]">
                    {sanitizeDisplayText(ext.name)}
                  </span>
                </div>
                <span className="shrink-0 font-mono text-[10px] text-[#81818B]">
                  {ext.scope}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="space-y-2">
        <div className="flex items-center gap-1.5 font-medium text-[#E8E8ED]">
          <Briefcase className="h-3.5 w-3.5 text-[#F6AD55]" />
          <span>Background Jobs ({jobs.length})</span>
        </div>
        {jobs.length === 0 ? (
          <div className="text-[11px] text-[#81818B]">
            No background jobs recorded in active session.
          </div>
        ) : (
          <ul className="max-h-36 space-y-1 overflow-y-auto">
            {jobs.map((job) => (
              <li
                key={job.job_id}
                className="flex items-center justify-between gap-2 rounded border border-[#303038] bg-[#09090B] px-2.5 py-1.5 text-[11px]"
              >
                <div className="min-w-0">
                  <div className="truncate font-mono text-[#E8E8ED]">
                    {job.tool ?? "job"} · {job.job_id}
                  </div>
                  <div className="truncate text-[10px] text-[#81818B]">
                    state: {job.state}
                    {job.summary ? ` · ${sanitizeDisplayText(job.summary)}` : ""}
                  </div>
                </div>
                {job.state === "running" && runActive && (
                  <button
                    type="button"
                    onClick={() => void handleCancelJob(job.job_id)}
                    className="inline-flex shrink-0 items-center gap-1 rounded border border-[#F87171]/40 bg-[#F87171]/10 px-2 py-0.5 text-[10px] text-[#F87171]"
                  >
                    <XCircle className="h-3 w-3" />
                    Cancel
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
