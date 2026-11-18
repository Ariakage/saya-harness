import { Check, Database, Search, X } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import {
  commands,
  type MemoryQueryResultDto,
  type PreparedMemoryReviewDto,
} from "../bindings";

export interface MemoryDialogProps {
  open: boolean;
  runActive: boolean;
  onClose: () => void;
}

export function MemoryDialog({ open, runActive, onClose }: MemoryDialogProps) {
  const [queryCmd, setQueryCmd] = useState("list");
  const [queryResult, setQueryResult] = useState<MemoryQueryResultDto | null>(
    null
  );
  const [actionCmd, setActionCmd] = useState("");
  const [prepared, setPrepared] = useState<PreparedMemoryReviewDto | null>(null);
  const [confirmMsg, setConfirmMsg] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const runQuery = useCallback(async (cmd: string) => {
    setErrorMsg(null);
    const res = await commands.queryMemory(cmd);
    if (res.status === "ok") {
      setQueryResult(res.data);
    } else {
      setErrorMsg(`${res.error.code}: ${res.error.message}`);
    }
  }, []);

  useEffect(() => {
    if (!open) return;
    setPrepared(null);
    setConfirmMsg(null);
    void runQuery("list");
  }, [open, runQuery]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

  if (!open) {
    return null;
  }

  const handlePrepare = async () => {
    const trimmed = actionCmd.trim();
    if (!trimmed) return;
    setErrorMsg(null);
    setConfirmMsg(null);
    setBusy(true);
    try {
      const res = await commands.prepareMemoryAction(trimmed);
      if (res.status === "ok") {
        setPrepared(res.data);
      } else {
        setErrorMsg(`${res.error.code}: ${res.error.message}`);
      }
    } finally {
      setBusy(false);
    }
  };

  const handleConfirm = async () => {
    if (!prepared) return;
    setErrorMsg(null);
    setBusy(true);
    try {
      const res = await commands.confirmMemoryAction(prepared.action_id);
      if (res.status === "ok") {
        setConfirmMsg(`Applied memory operation ${prepared.operation_id}`);
        setPrepared(null);
        setActionCmd("");
        await runQuery(queryCmd);
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
      aria-label="Global Memory Manager"
      data-testid="memory-dialog"
      className="soyo-backdrop-enter fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4 backdrop-blur-xs"
    >
      <div className="soyo-dialog-enter flex max-h-[85vh] w-full max-w-[700px] flex-col overflow-hidden rounded-[14px] border border-[#2b2b2b] bg-[#181818] shadow-[0_24px_60px_rgba(0,0,0,0.65)]">
        <div className="flex items-center justify-between border-b border-[#232323] px-4 py-3">
          <div className="flex items-center gap-2">
            <Database className="h-4 w-4 text-[#d9a08e]" />
            <h2 className="text-[14px] font-semibold text-[#e8e8e8]">
              Global Memory Administration (Two-Phase Review)
            </h2>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close memory manager"
            className="rounded-[7px] p-1 text-[#8a8a8a] transition-colors hover:bg-[#222222] hover:text-[#e8e8e8]"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="flex-1 space-y-4 overflow-y-auto p-4 text-xs">
          {errorMsg && (
            <div className="rounded-[8px] border border-[#f87171]/35 bg-[#1c1515] px-3 py-2 text-[#f87171]">
              {errorMsg}
            </div>
          )}
          {confirmMsg && (
            <div className="rounded-[8px] border border-[#4ade80]/35 bg-[#161c17] px-3 py-2 text-[#4ade80]">
              {confirmMsg}
            </div>
          )}

          <div className="space-y-2">
            <div className="flex gap-2">
              <input
                type="text"
                aria-label="Memory query command"
                value={queryCmd}
                onChange={(e) => setQueryCmd(e.target.value)}
                placeholder="list | search <query> | show <id> | sources | history"
                className="soyo-input flex-1 px-2.5 py-1.5 font-mono text-xs"
              />
              <button
                type="button"
                onClick={() => void runQuery(queryCmd)}
                className="soyo-pill-btn font-medium text-[#e8e8e8]"
              >
                <Search className="h-3.5 w-3.5 text-[#8a8a8a]" />
                Query
              </button>
            </div>
            {queryResult && (
              <pre className="max-h-56 overflow-auto rounded-[10px] border border-[#262626] bg-[#141414] p-3 font-mono text-[11px] text-[#c9d1d9]">
                {JSON.stringify(queryResult.data, null, 2)}
              </pre>
            )}
          </div>

          <div className="space-y-2 rounded-[10px] border border-[#262626] bg-[#141414] p-3">
            <div className="font-medium text-[#e8e8e8]">
              Prepare Memory Mutation / Export
            </div>
            <div className="flex gap-2">
              <input
                type="text"
                aria-label="Prepare memory command"
                value={actionCmd}
                disabled={runActive || busy}
                onChange={(e) => setActionCmd(e.target.value)}
                placeholder="e.g. forget <fact_id> | correct <fact_id> <text> | export <path>"
                className="soyo-input flex-1 px-2.5 py-1.5 font-mono text-xs"
              />
              <button
                type="button"
                disabled={runActive || busy || !actionCmd.trim()}
                onClick={() => void handlePrepare()}
                className="rounded-[8px] bg-[#d9a08e] px-3 py-1.5 font-semibold text-[#1c120e] transition-colors hover:bg-[#e2a68f] disabled:opacity-40"
              >
                Prepare Review
              </button>
            </div>

            {prepared && (
              <div className="soyo-panel-enter mt-2 space-y-2 rounded-[9px] border border-[#4a3f2a] bg-[#1c1a15] p-3">
                <div className="flex items-center justify-between text-[11px]">
                  <span className="font-mono text-[#e8d9b0]">
                    op: {prepared.operation_id} · rev: {prepared.expected_revision}
                  </span>
                </div>
                <pre className="max-h-40 overflow-auto rounded-[8px] border border-[#2e2820] bg-[#121110] p-2 font-mono text-[11px] text-[#d9d9d9]">
                  {JSON.stringify(prepared.review, null, 2)}
                </pre>
                <div className="flex justify-end gap-2">
                  <button
                    type="button"
                    onClick={() => setPrepared(null)}
                    className="soyo-pill-btn text-[#c9c9c9]"
                  >
                    Discard
                  </button>
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => void handleConfirm()}
                    className="soyo-pill-btn font-semibold text-[#4ade80]"
                  >
                    <Check className="h-3.5 w-3.5" />
                    Confirm Apply
                  </button>
                </div>
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
