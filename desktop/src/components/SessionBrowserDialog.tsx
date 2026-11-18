import { AlertTriangle, ChevronLeft, ChevronRight, History, X } from "lucide-react";
import { useEffect, useState } from "react";
import { commands, type SessionItemDto_Serialize } from "../bindings";
import { WORK_MODE_STYLES, sanitizeDisplayText } from "../lib/sanitize";

export interface SessionBrowserDialogProps {
  open: boolean;
  activeSessionId: string | null;
  runActive: boolean;
  configChangeSessionId: string | null;
  onClose: () => void;
  onResume: (
    sessionId: string,
    acceptConfigChange?: boolean
  ) => Promise<"ok" | "needs_confirm" | "error">;
  onClearConfigChange: () => void;
}

export function SessionBrowserDialog({
  open,
  activeSessionId,
  runActive,
  configChangeSessionId,
  onClose,
  onResume,
  onClearConfigChange,
}: SessionBrowserDialogProps) {
  const [page, setPage] = useState(1);
  const [items, setItems] = useState<SessionItemDto_Serialize[]>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!open) return;
    let mounted = true;
    setLoading(true);
    commands
      .listSessions(page)
      .then((res) => {
        if (mounted && res.status === "ok") {
          setItems(res.data.items);
        }
      })
      .finally(() => {
        if (mounted) setLoading(false);
      });
    return () => {
      mounted = false;
    };
  }, [open, page]);

  if (!open) {
    return null;
  }

  const handleResume = async (id: string, acceptConfigChange = false) => {
    const outcome = await onResume(id, acceptConfigChange);
    if (outcome === "ok") {
      onClose();
    }
  };

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Session Browser"
      data-testid="session-browser-dialog"
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4 backdrop-blur-xs"
    >
      <div className="flex max-h-[80vh] w-full max-w-2xl flex-col rounded-xl border border-[#303038] bg-[#141416] shadow-2xl">
        <div className="flex items-center justify-between border-b border-[#303038] px-4 py-3">
          <div className="flex items-center gap-2">
            <History className="h-4 w-4 text-[#58A6FF]" />
            <h2 className="text-sm font-semibold text-[#E8E8ED]">
              Resume Session
            </h2>
          </div>
          <button
            type="button"
            onClick={() => {
              onClearConfigChange();
              onClose();
            }}
            aria-label="Close session browser"
            className="rounded p-1 text-[#81818B] hover:bg-[#202024] hover:text-[#E8E8ED]"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        {configChangeSessionId && (
          <div className="border-b border-[#F6AD55]/40 bg-[#F6AD55]/10 px-4 py-3 text-xs text-[#F6AD55]">
            <div className="flex items-start gap-2">
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
              <div className="flex-1 space-y-2">
                <p>
                  Profile settings changed since session{" "}
                  <span className="font-mono">{configChangeSessionId}</span> was
                  saved. Confirm to resume with current configuration?
                </p>
                <div className="flex gap-2">
                  <button
                    type="button"
                    onClick={() =>
                      void handleResume(configChangeSessionId, true)
                    }
                    className="rounded bg-[#F6AD55] px-2.5 py-1 font-semibold text-[#09090B]"
                  >
                    Accept Config Change & Resume
                  </button>
                  <button
                    type="button"
                    onClick={onClearConfigChange}
                    className="rounded border border-[#303038] bg-[#141416] px-2.5 py-1 text-[#D4D4D8]"
                  >
                    Cancel
                  </button>
                </div>
              </div>
            </div>
          </div>
        )}

        <div className="flex-1 overflow-y-auto p-4">
          {loading ? (
            <div className="py-10 text-center text-xs text-[#81818B]">
              Loading sessions...
            </div>
          ) : items.length === 0 ? (
            <div className="py-10 text-center text-xs text-[#81818B]">
              No sessions found on this page.
            </div>
          ) : (
            <ul className="space-y-2">
              {items.map((item) => {
                const isCurrent = item.id === activeSessionId;
                const modeStyle = WORK_MODE_STYLES[item.work_mode];
                return (
                  <li
                    key={item.id}
                    className={`flex items-center justify-between gap-3 rounded-lg border px-3 py-2.5 text-xs ${
                      isCurrent
                        ? "border-[#58A6FF]/50 bg-[#58A6FF]/10"
                        : "border-[#303038] bg-[#09090B]"
                    }`}
                  >
                    <div className="min-w-0 flex-1 space-y-1">
                      <div className="flex items-center gap-2">
                        <span className="truncate font-medium text-[#E8E8ED]">
                          {sanitizeDisplayText(item.title ?? "Untitled session")}
                        </span>
                        <span
                          className={`rounded border px-1 py-0.5 text-[10px] font-semibold ${modeStyle.badgeClass}`}
                        >
                          {modeStyle.label}
                        </span>
                        {isCurrent && (
                          <span className="text-[10px] text-[#58A6FF]">
                            active
                          </span>
                        )}
                      </div>
                      <div className="truncate text-[11px] text-[#81818B]">
                        {sanitizeDisplayText(item.workspace)} · {item.profile}
                        {item.last_status ? ` · ${item.last_status}` : ""}
                      </div>
                    </div>

                    <button
                      type="button"
                      disabled={runActive}
                      onClick={() => void handleResume(item.id, false)}
                      className="shrink-0 rounded-md border border-[#303038] bg-[#202024] px-3 py-1.5 text-xs font-medium text-[#E8E8ED] hover:border-[#58A6FF] disabled:opacity-40"
                    >
                      {isCurrent ? "Current" : "Resume"}
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>

        <div className="flex items-center justify-between border-t border-[#303038] px-4 py-2.5 text-xs text-[#81818B]">
          <span>Page {page}</span>
          <div className="flex items-center gap-2">
            <button
              type="button"
              disabled={page <= 1}
              onClick={() => setPage((p) => Math.max(1, p - 1))}
              className="inline-flex items-center gap-1 rounded border border-[#303038] bg-[#202024] px-2 py-1 text-[#D4D4D8] disabled:opacity-40"
            >
              <ChevronLeft className="h-3.5 w-3.5" />
              Prev
            </button>
            <button
              type="button"
              disabled={items.length < 20}
              onClick={() => setPage((p) => p + 1)}
              className="inline-flex items-center gap-1 rounded border border-[#303038] bg-[#202024] px-2 py-1 text-[#D4D4D8] disabled:opacity-40"
            >
              Next
              <ChevronRight className="h-3.5 w-3.5" />
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
