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
    if (modRes.status === "ok" && Array.isArray(modRes.data)) setModules(modRes.data);
    if (extRes.status === "ok" && Array.isArray(extRes.data)) setExtensions(extRes.data);
    if (jobRes.status === "ok" && Array.isArray(jobRes.data)) setJobs(jobRes.data);
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
        <div className="rounded-[8px] border border-[#f87171]/35 bg-[#1c1515] px-2.5 py-1.5 text-[11px] text-[#f87171]">
          {statusMessage}
        </div>
      )}

      <section className="space-y-2">
        <div className="flex items-center gap-1.5 font-medium text-[#e8e8e8]">
          <Blocks className="h-3.5 w-3.5 text-[#d9a08e]" />
          <span>Runtime Modules</span>
        </div>
        {modules.length === 0 ? (
          <div className="text-[11px] text-[#6e6e6e]">
            No module status available.
          </div>
        ) : (
          <ul className="space-y-2">
            {modules.map((mod) => (
              <li
                key={mod.module}
                className="flex items-start justify-between gap-3 rounded-[9px] border border-[#262626] bg-[#1a1a1a] p-2.5"
              >
                <div className="min-w-0 space-y-1">
                  <div className="flex items-center gap-2">
                    <span className="font-mono font-semibold text-[#e8e8e8]">
                      {mod.module}
                    </span>
                    <span className="soyo-pill text-[10px] text-[#7a7a7a]">
                      {mod.source}
                    </span>
                  </div>
                  <div className="text-[11px] text-[#8a8a8a]">
                    {sanitizeDisplayText(mod.description)}
                  </div>
                  {mod.issue && (
                    <div className="flex items-center gap-1 text-[11px] text-[#fac454]">
                      <AlertTriangle className="h-3 w-3 shrink-0" />
                      <span>{sanitizeDisplayText(mod.issue)}</span>
                    </div>
                  )}
                </div>
                <button
                  type="button"
                  disabled={runActive}
                  onClick={() => void handleToggleModule(mod)}
                  className={`soyo-pill-btn shrink-0 ${
                    mod.enabled ? "text-[#4ade80]" : "text-[#8a8a8a]"
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
        <div className="flex items-center gap-1.5 font-medium text-[#e8e8e8]">
          <Puzzle className="h-3.5 w-3.5 text-[#a096eb]" />
          <span>Extensions & Project Discovery ({extensions.length})</span>
        </div>
        {extensions.length === 0 ? (
          <div className="text-[11px] text-[#6e6e6e]">
            No discovered rules, skills, or plugins in current workspace.
          </div>
        ) : (
          <ul className="max-h-40 space-y-1 overflow-y-auto">
            {extensions.map((ext) => (
              <li
                key={`${ext.kind}:${ext.id}`}
                className="flex items-center justify-between gap-2 rounded-[8px] border border-[#262626] bg-[#1a1a1a] px-2.5 py-1.5 text-[11px]"
              >
                <div className="min-w-0">
                  <span className="mr-1.5 rounded bg-[#222222] px-1.5 py-0.5 font-mono text-[10px] text-[#7aa2f7]">
                    {ext.kind}
                  </span>
                  <span className="font-medium text-[#e8e8e8]">
                    {sanitizeDisplayText(ext.name)}
                  </span>
                </div>
                <span className="shrink-0 font-mono text-[10px] text-[#6e6e6e]">
                  {ext.scope}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="space-y-2">
        <div className="flex items-center gap-1.5 font-medium text-[#e8e8e8]">
          <Briefcase className="h-3.5 w-3.5 text-[#fac454]" />
          <span>Background Jobs ({jobs.length})</span>
        </div>
        {jobs.length === 0 ? (
          <div className="text-[11px] text-[#6e6e6e]">
            No background jobs recorded in active session.
          </div>
        ) : (
          <ul className="max-h-36 space-y-1 overflow-y-auto">
            {jobs.map((job) => (
              <li
                key={job.job_id}
                className="flex items-center justify-between gap-2 rounded-[8px] border border-[#262626] bg-[#1a1a1a] px-2.5 py-1.5 text-[11px]"
              >
                <div className="min-w-0">
                  <div className="truncate font-mono text-[#e8e8e8]">
                    {job.tool ?? "job"} · {job.job_id}
                  </div>
                  <div className="truncate text-[10px] text-[#8a8a8a]">
                    state: {job.state}
                    {job.summary ? ` · ${sanitizeDisplayText(job.summary)}` : ""}
                  </div>
                </div>
                {job.state === "running" && runActive && (
                  <button
                    type="button"
                    onClick={() => void handleCancelJob(job.job_id)}
                    className="soyo-pill-btn shrink-0 text-[10px] text-[#f87171]"
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
