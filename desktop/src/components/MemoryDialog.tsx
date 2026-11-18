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
  const [queryResult, setQueryResult] = useState<MemoryQueryResultDto | null>(null);
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
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4 backdrop-blur-xs"
    >
      <div className="flex max-h-[85vh] w-full max-w-3xl flex-col rounded-xl border border-[#303038] bg-[#141416] shadow-2xl">
        <div className="flex items-center justify-between border-b border-[#303038] px-4 py-3">
          <div className="flex items-center gap-2">
            <Database className="h-4 w-4 text-[#BB9AF7]" />
            <h2 className="text-sm font-semibold text-[#E8E8ED]">
              Global Memory Administration (Two-Phase Review)
            </h2>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close memory manager"
            className="rounded p-1 text-[#81818B] hover:bg-[#202024] hover:text-[#E8E8ED]"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="flex-1 space-y-4 overflow-y-auto p-4 text-xs">
          {errorMsg && (
            <div className="rounded border border-[#F87171]/40 bg-[#F87171]/10 px-3 py-2 text-[#F87171]">
              {errorMsg}
            </div>
          )}
          {confirmMsg && (
            <div className="rounded border border-[#7DCB9B]/40 bg-[#7DCB9B]/10 px-3 py-2 text-[#7DCB9B]">
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
                className="flex-1 rounded-md border border-[#303038] bg-[#202024] px-2.5 py-1.5 font-mono text-xs text-[#E8E8ED]"
              />
              <button
                type="button"
                onClick={() => void runQuery(queryCmd)}
                className="inline-flex items-center gap-1 rounded-md border border-[#303038] bg-[#202024] px-3 py-1.5 font-medium text-[#E8E8ED] hover:border-[#58A6FF]"
              >
                <Search className="h-3.5 w-3.5" />
                Query
              </button>
            </div>
            {queryResult && (
              <pre className="max-h-56 overflow-auto rounded-md border border-[#303038] bg-[#09090B] p-3 font-mono text-[11px] text-[#D4D4D8]">
                {JSON.stringify(queryResult.data, null, 2)}
              </pre>
            )}
          </div>

          <div className="space-y-2 rounded-md border border-[#303038] bg-[#09090B] p-3">
            <div className="font-medium text-[#E8E8ED]">
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
                className="flex-1 rounded-md border border-[#303038] bg-[#202024] px-2.5 py-1.5 font-mono text-xs text-[#E8E8ED] disabled:opacity-50"
              />
              <button
                type="button"
                disabled={runActive || busy || !actionCmd.trim()}
                onClick={() => void handlePrepare()}
                className="rounded-md bg-[#BB9AF7] px-3 py-1.5 font-semibold text-[#09090B] disabled:opacity-40"
              >
                Prepare Review
              </button>
            </div>

            {prepared && (
              <div className="mt-2 space-y-2 rounded border border-[#BB9AF7]/40 bg-[#141416] p-3">
                <div className="flex items-center justify-between text-[11px]">
                  <span className="font-mono text-[#BB9AF7]">
                    op: {prepared.operation_id} · rev: {prepared.expected_revision}
                  </span>
                </div>
                <pre className="max-h-40 overflow-auto rounded bg-[#09090B] p-2 font-mono text-[11px] text-[#D4D4D8]">
                  {JSON.stringify(prepared.review, null, 2)}
                </pre>
                <div className="flex justify-end gap-2">
                  <button
                    type="button"
                    onClick={() => setPrepared(null)}
                    className="rounded border border-[#303038] bg-[#202024] px-3 py-1 text-[#D4D4D8]"
                  >
                    Discard
                  </button>
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => void handleConfirm()}
                    className="inline-flex items-center gap-1 rounded bg-[#7DCB9B] px-3 py-1 font-semibold text-[#09090B]"
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
