import { AlertTriangle, ChevronLeft, ChevronRight, Folder, History, X } from "lucide-react";
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

function projectName(workspace: string): string {
  const cleaned = workspace.replace(/\/+$/, "");
  return cleaned.split("/").pop() || workspace || "workspace";
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
  const [selectedId, setSelectedId] = useState<string | null>(activeSessionId);
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
          setSelectedId((prev) =>
            prev && res.data.items.some((i) => i.id === prev)
              ? prev
              : activeSessionId ?? res.data.items[0]?.id ?? null
          );
        }
      })
      .finally(() => {
        if (mounted) setLoading(false);
      });
    return () => {
      mounted = false;
    };
  }, [open, page, activeSessionId]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClearConfigChange();
        onClose();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onClearConfigChange, onClose]);

  if (!open) {
    return null;
  }

  const handleResume = async (id: string, acceptConfigChange = false) => {
    const outcome = await onResume(id, acceptConfigChange);
    if (outcome === "ok") {
      onClose();
    }
  };

  // Group sessions by project folder (§2 / Frame 06)
  const grouped = new Map<string, SessionItemDto_Serialize[]>();
  for (const item of items) {
    const key = projectName(item.workspace);
    const list = grouped.get(key) ?? [];
    list.push(item);
    grouped.set(key, list);
  }
  const selectedItem =
    items.find((i) => i.id === selectedId) ?? items[0] ?? null;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Session Browser"
      data-testid="session-browser-dialog"
      className="soyo-backdrop-enter fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4 backdrop-blur-xs"
    >
      <div className="soyo-dialog-enter flex max-h-[82vh] w-full max-w-[740px] flex-col overflow-hidden rounded-[14px] border border-[#2b2b2b] bg-[#181818] shadow-[0_24px_60px_rgba(0,0,0,0.65)]">
        <div className="flex items-center justify-between border-b border-[#232323] px-4 py-3">
          <div className="flex items-center gap-2">
            <History className="h-4 w-4 text-[#d9a08e]" />
            <h2 className="text-[14px] font-semibold text-[#e8e8e8]">
              Sessions & Project History
            </h2>
          </div>
          <button
            type="button"
            onClick={() => {
              onClearConfigChange();
              onClose();
            }}
            aria-label="Close session browser"
            className="rounded-[7px] p-1 text-[#8a8a8a] transition-colors hover:bg-[#222222] hover:text-[#e8e8e8]"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        {configChangeSessionId && (
          <div className="border-b border-[#4a3f2a] bg-[#1c1a15] px-4 py-3 text-xs text-[#e8d9b0]">
            <div className="flex items-start gap-2">
              <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-[#fac454]" />
              <div className="flex-1 space-y-2">
                <p>
                  Profile settings changed since session{" "}
                  <span className="font-mono text-[#7aa2f7]">
                    {configChangeSessionId}
                  </span>{" "}
                  was saved. Confirm to resume with current configuration?
                </p>
                <div className="flex gap-2">
                  <button
                    type="button"
                    onClick={() =>
                      void handleResume(configChangeSessionId, true)
                    }
                    className="soyo-pill-btn text-[#d9a08e]"
                  >
                    Accept Config Change & Resume
                  </button>
                  <button
                    type="button"
                    onClick={onClearConfigChange}
                    className="soyo-pill-btn text-[#c9c9c9]"
                  >
                    Cancel
                  </button>
                </div>
              </div>
            </div>
          </div>
        )}

        <div className="flex min-h-0 flex-1 overflow-hidden">
          {/* Left: Project-grouped list */}
          <div className="flex-1 overflow-y-auto border-r border-[#232323] p-3">
            {loading ? (
              <div className="py-10 text-center text-xs text-[#6e6e6e]">
                Loading sessions...
              </div>
            ) : items.length === 0 ? (
              <div className="py-10 text-center text-xs text-[#6e6e6e]">
                No sessions found on this page.
              </div>
            ) : (
              <div className="space-y-3">
                {Array.from(grouped.entries()).map(([proj, groupItems]) => (
                  <div key={proj} className="space-y-1">
                    <div className="flex items-center justify-between px-2 py-1 text-[11px] text-[#6b6b6b]">
                      <span className="inline-flex items-center gap-1.5 font-medium text-[#8a8a8a]">
                        <Folder className="h-3 w-3" />
                        {proj}
                      </span>
                      <span>{groupItems.length}</span>
                    </div>
                    <ul className="space-y-1">
                      {groupItems.map((item) => {
                        const isCurrent = item.id === activeSessionId;
                        const isSelected = item.id === selectedItem?.id;
                        const modeStyle = WORK_MODE_STYLES[item.work_mode];
                        return (
                          <li
                            key={item.id}
                            onClick={() => setSelectedId(item.id)}
                            className={`flex cursor-pointer items-center justify-between gap-3 rounded-[8px] border px-3 py-2 text-xs transition-colors ${
                              isSelected
                                ? "border-[#2e2e2e] bg-[#1c1c1c]"
                                : "border-transparent hover:bg-[#1a1a1a]"
                            }`}
                          >
                            <div className="min-w-0 flex-1 space-y-1">
                              <div className="flex items-center gap-2">
                                <span className="truncate font-medium text-[#e8e8e8]">
                                  {sanitizeDisplayText(
                                    item.title ?? "Untitled session"
                                  )}
                                </span>
                                <span
                                  className={`soyo-pill py-0 text-[10.5px] ${modeStyle.badgeClass}`}
                                >
                                  {modeStyle.label}
                                </span>
                                {isCurrent && (
                                  <span className="text-[10.5px] text-[#d9a08e]">
                                    active
                                  </span>
                                )}
                              </div>
                              <div className="truncate text-[11px] text-[#6e6e6e]">
                                {item.profile}
                                {item.last_status ? ` · ${item.last_status}` : ""}
                              </div>
                            </div>

                            <button
                              type="button"
                              disabled={runActive}
                              onClick={(e) => {
                                e.stopPropagation();
                                void handleResume(item.id, false);
                              }}
                              className="soyo-pill-btn shrink-0 text-[#e8e8e8] disabled:opacity-40"
                            >
                              {isCurrent ? "Current" : "Resume"}
                            </button>
                          </li>
                        );
                      })}
                    </ul>
                  </div>
                ))}
              </div>
            )}
          </div>

          {/* Right: Session details summary (Frame 06) */}
          <div className="hidden w-[250px] shrink-0 flex-col justify-between bg-[#141414] p-4 text-xs sm:flex">
            {selectedItem ? (
              <div key={selectedItem.id} className="soyo-panel-enter space-y-3">
                <div className="text-[11px] uppercase tracking-wider text-[#6e6e6e]">
                  Session Details
                </div>
                <div className="text-[13px] font-semibold text-[#e8e8e8]">
                  {sanitizeDisplayText(
                    selectedItem.title ?? "Untitled session"
                  )}
                </div>
                <div className="space-y-1.5 rounded-[9px] border border-[#262626] bg-[#1a1a1a] p-2.5 text-[11.5px]">
                  <div className="flex justify-between gap-2">
                    <span className="text-[#7a7a7a]">Project</span>
                    <span className="font-mono text-[#e8e8e8]">
                      {projectName(selectedItem.workspace)}
                    </span>
                  </div>
                  <div className="flex justify-between gap-2">
                    <span className="text-[#7a7a7a]">Profile</span>
                    <span className="font-mono text-[#c9c9c9]">
                      {selectedItem.profile}
                    </span>
                  </div>
                  <div className="flex justify-between gap-2">
                    <span className="text-[#7a7a7a]">Mode</span>
                    <span
                      style={{
                        color: WORK_MODE_STYLES[selectedItem.work_mode].color,
                      }}
                    >
                      {WORK_MODE_STYLES[selectedItem.work_mode].label}
                    </span>
                  </div>
                  <div className="flex justify-between gap-2">
                    <span className="text-[#7a7a7a]">Status</span>
                    <span className="text-[#8aa8d8]">
                      {selectedItem.last_status ?? "saved"}
                    </span>
                  </div>
                </div>
                <div className="truncate font-mono text-[10.5px] text-[#6e6e6e]">
                  {sanitizeDisplayText(selectedItem.workspace)}
                </div>
              </div>
            ) : (
              <div className="text-[#6e6e6e]">Select a session to inspect.</div>
            )}
          </div>
        </div>

        <div className="flex items-center justify-between border-t border-[#232323] px-4 py-2.5 text-xs text-[#6e6e6e]">
          <span>Page {page}</span>
          <div className="flex items-center gap-2">
            <button
              type="button"
              disabled={page <= 1}
              onClick={() => setPage((p) => Math.max(1, p - 1))}
              className="soyo-pill-btn text-[#c9c9c9] disabled:opacity-40"
            >
              <ChevronLeft className="h-3.5 w-3.5" />
              Prev
            </button>
            <button
              type="button"
              disabled={items.length < 20}
              onClick={() => setPage((p) => p + 1)}
              className="soyo-pill-btn text-[#c9c9c9] disabled:opacity-40"
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
