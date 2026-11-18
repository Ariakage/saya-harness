import { isTauri } from "@tauri-apps/api/core";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { Download, X } from "lucide-react";
import { useState } from "react";
import {
  commands,
  type ExportFormatDto,
  type ExportResultDto,
  type ExportScopeDto,
} from "../bindings";

export interface ExportDialogProps {
  open: boolean;
  sessionId: string | null;
  workspace: string;
  onClose: () => void;
}

export function ExportDialog({
  open,
  sessionId,
  workspace,
  onClose,
}: ExportDialogProps) {
  const [targetPath, setTargetPath] = useState("session-export.md");
  const [format, setFormat] = useState<ExportFormatDto>("markdown");
  const [scope, setScope] = useState<ExportScopeDto>("transcript");
  const [includeTools, setIncludeTools] = useState(true);
  const [result, setResult] = useState<ExportResultDto | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  if (!open) {
    return null;
  }

  const handleExport = async () => {
    if (!targetPath.trim()) return;
    setErrorMsg(null);
    setResult(null);
    setBusy(true);
    try {
      const res = await commands.exportSession(
        sessionId,
        targetPath.trim(),
        format,
        scope,
        includeTools
      );
      if (res.status === "ok") {
        setResult(res.data);
      } else {
        setErrorMsg(`${res.error.code}: ${res.error.message}`);
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Export Session"
      data-testid="export-dialog"
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4 backdrop-blur-xs"
    >
      <div className="w-full max-w-lg overflow-hidden rounded-[14px] border border-[#2b2b2b] bg-[#181818] shadow-[0_24px_60px_rgba(0,0,0,0.65)]">
        <div className="flex items-center justify-between border-b border-[#232323] px-4 py-3">
          <div className="flex items-center gap-2">
            <Download className="h-4 w-4 text-[#d9a08e]" />
            <h2 className="text-[14px] font-semibold text-[#e8e8e8]">
              Export Session Transcript
            </h2>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close export dialog"
            className="rounded-[7px] p-1 text-[#8a8a8a] hover:bg-[#222222] hover:text-[#e8e8e8]"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="space-y-3 p-4 text-xs">
          {errorMsg && (
            <div className="rounded-[8px] border border-[#f87171]/35 bg-[#1c1515] px-3 py-2 text-[#f87171]">
              {errorMsg}
            </div>
          )}

          {result && (
            <div className="space-y-1 rounded-[9px] border border-[#4ade80]/35 bg-[#161c17] p-2.5 text-[#4ade80]">
              <div className="font-medium">Export published</div>
              <div className="truncate font-mono text-[11px]">
                {result.path} ({result.bytes} bytes · {result.format})
              </div>
            </div>
          )}

          <div className="space-y-1">
            <label
              htmlFor="export-target-path"
              className="block text-[12px] text-[#a3a3a3]"
            >
              Output File Path (relative to {workspace || "workspace"} or
              absolute)
            </label>
            <div className="flex gap-2">
              <input
                id="export-target-path"
                type="text"
                value={targetPath}
                onChange={(e) => setTargetPath(e.target.value)}
                className="flex-1 rounded-[8px] border border-[#2b2b2b] bg-[#1e1e1e] px-2.5 py-1.5 font-mono text-xs text-[#e8e8e8]"
              />
              {isTauri() && (
                <button
                  type="button"
                  title="Choose output location"
                  onClick={() => {
                    void saveDialog({
                      defaultPath: targetPath || undefined,
                      title: "Export session transcript",
                      filters: [
                        format === "json"
                          ? { name: "JSON", extensions: ["json"] }
                          : { name: "Markdown", extensions: ["md"] },
                      ],
                    }).then((picked) => {
                      if (picked) setTargetPath(picked);
                    });
                  }}
                  className="devin-pill-btn shrink-0 font-medium text-[#c9c9c9]"
                >
                  Browse…
                </button>
              )}
            </div>
          </div>

          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1">
              <label
                htmlFor="export-format-select"
                className="block text-[12px] text-[#a3a3a3]"
              >
                Format
              </label>
              <select
                id="export-format-select"
                value={format}
                onChange={(e) => {
                  const next = e.target.value as ExportFormatDto;
                  setFormat(next);
                  setTargetPath((prev) =>
                    prev.replace(/\.(md|json)$/i, next === "json" ? ".json" : ".md")
                  );
                }}
                className="w-full rounded-[8px] border border-[#2b2b2b] bg-[#1e1e1e] px-2 py-1.5 text-xs text-[#e8e8e8]"
              >
                <option value="markdown" className="bg-[#181818]">
                  Markdown (.md)
                </option>
                <option value="json" className="bg-[#181818]">
                  JSON v1 (.json)
                </option>
              </select>
            </div>

            <div className="space-y-1">
              <label
                htmlFor="export-scope-select"
                className="block text-[12px] text-[#a3a3a3]"
              >
                Scope
              </label>
              <select
                id="export-scope-select"
                value={scope}
                onChange={(e) => setScope(e.target.value as ExportScopeDto)}
                className="w-full rounded-[8px] border border-[#2b2b2b] bg-[#1e1e1e] px-2 py-1.5 text-xs text-[#e8e8e8]"
              >
                <option value="transcript" className="bg-[#181818]">
                  Transcript
                </option>
                <option value="full" className="bg-[#181818]">
                  Full (with events/metrics)
                </option>
              </select>
            </div>
          </div>

          <label className="inline-flex items-center gap-2 text-[#c9c9c9]">
            <input
              type="checkbox"
              checked={includeTools}
              onChange={(e) => setIncludeTools(e.target.checked)}
            />
            Include tool arguments and result bodies
          </label>
        </div>

        <div className="flex justify-end gap-2 border-t border-[#232323] px-4 py-3 text-xs">
          <button
            type="button"
            onClick={onClose}
            className="devin-pill-btn text-[#c9c9c9]"
          >
            Close
          </button>
          <button
            type="button"
            disabled={busy || !sessionId || !targetPath.trim()}
            onClick={() => void handleExport()}
            className="rounded-[8px] bg-[#d9a08e] px-3.5 py-1.5 font-semibold text-[#1c120e] disabled:opacity-40"
          >
            {busy ? "Exporting..." : "Publish Export"}
          </button>
        </div>
      </div>
    </div>
  );
}
