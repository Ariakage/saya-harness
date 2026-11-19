import { AlertTriangle, ChevronLeft, ChevronRight, Folder, History, X } from "lucide-react";
import { useEffect, useState } from "react";
import { commands, type SessionItemDto_Serialize } from "../bindings";
import { getMessages } from "../lib/i18n";
import { WORK_MODE_STYLES, sanitizeDisplayText } from "../lib/sanitize";
import { useAppStore } from "../store/useAppStore";
import { SquaresLoader } from "./SquaresLoader";
import { Button } from "./ui/Button";
import { DialogShell } from "./ui/DialogShell";
import { Pill } from "./ui/Pill";

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

function projectName(workspace: string, fallback: string): string {
  const cleaned = workspace.replace(/\/+$/, "");
  return cleaned.split("/").pop() || workspace || fallback;
}

type SessionStatusFilter = "all" | "active" | "completed" | "failed";

export function SessionBrowserDialog({
  open,
  activeSessionId,
  runActive,
  configChangeSessionId,
  onClose,
  onResume,
  onClearConfigChange,
}: SessionBrowserDialogProps) {
  const locale = useAppStore((s) => s.locale);
  const t = getMessages(locale);
  const [page, setPage] = useState(1);
  const [items, setItems] = useState<SessionItemDto_Serialize[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(activeSessionId);
  const [loading, setLoading] = useState(false);
  const [projectFilter, setProjectFilter] = useState<string>("all");
  const [statusFilter, setStatusFilter] = useState<SessionStatusFilter>("all");

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

  if (!open) {
    return null;
  }

  const closeDialog = () => {
    onClearConfigChange();
    onClose();
  };

  const handleResume = async (id: string, acceptConfigChange = false) => {
    const outcome = await onResume(id, acceptConfigChange);
    if (outcome === "ok") {
      onClose();
    }
  };

  const projects = Array.from(
    new Set(
      items.map((item) =>
        projectName(item.workspace, t.sessionBrowser.workspaceFallback)
      )
    )
  );

  const filteredItems = items.filter((item) => {
    const proj = projectName(item.workspace, t.sessionBrowser.workspaceFallback);
    if (projectFilter !== "all" && proj !== projectFilter) {
      return false;
    }
    if (statusFilter === "all") return true;
    const raw = (item.last_status ?? "").toLowerCase();
    if (statusFilter === "active") {
      return (
        item.id === activeSessionId ||
        raw.includes("active") ||
        raw.includes("run")
      );
    }
    if (statusFilter === "completed") {
      return (
        raw.includes("complete") ||
        raw.includes("done") ||
        raw.includes("finish") ||
        raw === "" ||
        raw === "saved"
      );
    }
    return (
      raw.includes("fail") ||
      raw.includes("error") ||
      raw.includes("cancel")
    );
  });

  // Group sessions by project folder (§8.6)
  const grouped = new Map<string, SessionItemDto_Serialize[]>();
  for (const item of filteredItems) {
    const key = projectName(item.workspace, t.sessionBrowser.workspaceFallback);
    const list = grouped.get(key) ?? [];
    list.push(item);
    grouped.set(key, list);
  }
  const selectedItem =
    filteredItems.find((i) => i.id === selectedId) ?? filteredItems[0] ?? null;

  return (
    <DialogShell
      open={open}
      onClose={closeDialog}
      ariaLabel={t.sessionBrowser.title}
      testId="session-browser-dialog"
      size="md"
      panelClassName="flex max-h-[82vh] max-w-[740px] flex-col"
    >
      <div className="flex items-center justify-between border-b border-[var(--border-subtle)] px-4 py-3">
        <div className="flex items-center gap-2">
          <History className="h-4 w-4 text-[var(--accent-text)]" strokeWidth={1.75} />
          <h2 className="text-[14px] font-semibold text-[var(--text-primary)]">
            {t.sessionBrowser.title}
          </h2>
        </div>
        <button
          type="button"
          onClick={closeDialog}
          aria-label={t.a11y.closeSessionBrowser}
          className="rounded-[var(--radius-sm)] p-1 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
        >
          <X className="h-4 w-4" strokeWidth={1.75} />
        </button>
      </div>

      {/* Filters bar */}
      <div className="flex flex-wrap items-center gap-2 border-b border-[var(--border-subtle)] bg-[var(--surface-app)] px-4 py-2 text-xs">
        <select
          aria-label={t.a11y.workspaceFilter}
          value={projectFilter}
          onChange={(e) => setProjectFilter(e.target.value)}
          className="soyo-select px-2.5 py-1 text-[11px]"
        >
          <option value="all" className="bg-[var(--surface-overlay)]">
            {t.extra.filterAllProjects}
          </option>
          {projects.map((proj) => (
            <option key={proj} value={proj} className="bg-[var(--surface-overlay)]">
              {proj}
            </option>
          ))}
        </select>

        <select
          aria-label={t.a11y.statusFilter}
          value={statusFilter}
          onChange={(e) => setStatusFilter(e.target.value as SessionStatusFilter)}
          className="soyo-select px-2.5 py-1 text-[11px]"
        >
          <option value="all" className="bg-[var(--surface-overlay)]">
            {t.extra.filterAllStatuses}
          </option>
          <option value="active" className="bg-[var(--surface-overlay)]">
            {t.extra.statusActive}
          </option>
          <option value="completed" className="bg-[var(--surface-overlay)]">
            {t.extra.statusCompleted}
          </option>
          <option value="failed" className="bg-[var(--surface-overlay)]">
            {t.extra.statusFailed}
          </option>
        </select>
      </div>

      {configChangeSessionId && (
        <div className="border-b border-[var(--warning)] bg-[var(--warning-surface)] px-4 py-3 text-xs text-[var(--text-primary)]">
          <div className="flex items-start gap-2">
            <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-[var(--warning)]" strokeWidth={1.75} />
            <div className="flex-1 space-y-2">
              <p>
                {t.sessionBrowser.configChangedPrefix}{" "}
                <span className="font-mono text-[var(--info)]">
                  {configChangeSessionId}
                </span>{" "}
                {t.sessionBrowser.configChangedSuffix}
              </p>
              <div className="flex gap-2">
                <Button
                  variant="primary"
                  size="sm"
                  onClick={() => void handleResume(configChangeSessionId, true)}
                >
                  {t.sessionBrowser.acceptAndResume}
                </Button>
                <Button variant="secondary" size="sm" onClick={onClearConfigChange}>
                  {t.sessionBrowser.cancel}
                </Button>
              </div>
            </div>
          </div>
        </div>
      )}

      <div className="flex min-h-0 flex-1 overflow-hidden">
        {/* Left: Project-grouped list */}
        <div className="flex-1 overflow-y-auto border-r border-[var(--border-subtle)] p-3">
          {loading ? (
            <div className="flex items-center justify-center gap-2 py-10 text-xs text-[var(--text-tertiary)]">
              <SquaresLoader size={12} />
              <span>{t.sessionBrowser.loading}</span>
            </div>
          ) : filteredItems.length === 0 ? (
            <div className="py-10 text-center text-xs text-[var(--text-tertiary)]">
              {t.sessionBrowser.emptyPage}
            </div>
          ) : (
            <div className="space-y-3">
              {Array.from(grouped.entries()).map(([proj, groupItems]) => (
                <div key={proj} className="space-y-1">
                  <div className="flex items-center justify-between px-2 py-1 text-[11px] text-[var(--text-tertiary)]">
                    <span className="inline-flex items-center gap-1.5 font-medium text-[var(--text-secondary)]">
                      <Folder className="h-3 w-3" strokeWidth={1.75} />
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
                          className={`flex cursor-pointer items-center justify-between gap-3 rounded-[var(--radius-md)] border px-3 py-2 text-xs transition-colors ${
                            isSelected
                              ? "border-[var(--border)] bg-[var(--surface-raised)]"
                              : "border-transparent hover:bg-[var(--surface-hover)]"
                          }`}
                        >
                          <div className="min-w-0 flex-1 space-y-1">
                            <div className="flex items-center gap-2">
                              <span className="truncate font-medium text-[var(--text-primary)]">
                                {sanitizeDisplayText(
                                  item.title ?? t.sidebar.untitledSession
                                )}
                              </span>
                              <span className={`soyo-pill py-0 text-[11px] ${modeStyle.badgeClass}`}>
                                {modeStyle.label}
                              </span>
                              {isCurrent && (
                                <Pill tone="accent">{t.sessionBrowser.active}</Pill>
                              )}
                            </div>
                            <div className="truncate text-[11px] text-[var(--text-tertiary)]">
                              {item.profile}
                              {item.last_status ? ` · ${item.last_status}` : ""}
                            </div>
                          </div>

                          <Button
                            variant="secondary"
                            size="sm"
                            disabled={runActive}
                            onClick={(e) => {
                              e.stopPropagation();
                              void handleResume(item.id, false);
                            }}
                          >
                            {isCurrent ? t.sessionBrowser.current : t.sessionBrowser.resume}
                          </Button>
                        </li>
                      );
                    })}
                  </ul>
                </div>
              ))}
            </div>
          )}
        </div>

        {/* Right: Session details summary */}
        <div className="hidden w-[250px] shrink-0 flex-col justify-between bg-[var(--surface-app)] p-4 text-xs sm:flex">
          {selectedItem ? (
            <div key={selectedItem.id} className="soyo-panel-enter space-y-3">
              <div className="text-[11px] uppercase tracking-wider text-[var(--text-tertiary)]">
                {t.sessionBrowser.detailsHeader}
              </div>
              <div className="text-[13px] font-semibold text-[var(--text-primary)]">
                {sanitizeDisplayText(
                  selectedItem.title ?? t.sidebar.untitledSession
                )}
              </div>
              <div className="space-y-1.5 rounded-[var(--radius-md)] border border-[var(--border-subtle)] bg-[var(--surface-raised)] p-2.5 text-[12px]">
                <div className="flex justify-between gap-2">
                  <span className="text-[var(--text-tertiary)]">{t.sessionBrowser.project}</span>
                  <span className="font-mono text-[var(--text-primary)]">
                    {projectName(selectedItem.workspace, t.sessionBrowser.workspaceFallback)}
                  </span>
                </div>
                <div className="flex justify-between gap-2">
                  <span className="text-[var(--text-tertiary)]">{t.sessionBrowser.profile}</span>
                  <span className="font-mono text-[var(--text-secondary)]">
                    {selectedItem.profile}
                  </span>
                </div>
                <div className="flex justify-between gap-2">
                  <span className="text-[var(--text-tertiary)]">{t.sessionBrowser.mode}</span>
                  <span style={{ color: WORK_MODE_STYLES[selectedItem.work_mode].color }}>
                    {WORK_MODE_STYLES[selectedItem.work_mode].label}
                  </span>
                </div>
                <div className="flex justify-between gap-2">
                  <span className="text-[var(--text-tertiary)]">{t.sessionBrowser.status}</span>
                  <span className="text-[var(--info)]">
                    {selectedItem.last_status ?? t.sidebar.savedStatus}
                  </span>
                </div>
              </div>
              <div className="truncate font-mono text-[11px] text-[var(--text-tertiary)]">
                {sanitizeDisplayText(selectedItem.workspace)}
              </div>
            </div>
          ) : (
            <div className="text-[var(--text-tertiary)]">{t.sessionBrowser.selectToInspect}</div>
          )}
        </div>
      </div>

      <div className="flex items-center justify-between border-t border-[var(--border-subtle)] px-4 py-2.5 text-xs text-[var(--text-tertiary)]">
        <span>{t.sessionBrowser.pageLabel(page)}</span>
        <div className="flex items-center gap-2">
          <Button
            variant="secondary"
            size="sm"
            disabled={page <= 1}
            onClick={() => setPage((p) => Math.max(1, p - 1))}
          >
            <ChevronLeft className="h-3.5 w-3.5" strokeWidth={1.75} />
            {t.sessionBrowser.prev}
          </Button>
          <Button
            variant="secondary"
            size="sm"
            disabled={items.length < 20}
            onClick={() => setPage((p) => p + 1)}
          >
            {t.sessionBrowser.next}
            <ChevronRight className="h-3.5 w-3.5" strokeWidth={1.75} />
          </Button>
        </div>
      </div>
    </DialogShell>
  );
}
