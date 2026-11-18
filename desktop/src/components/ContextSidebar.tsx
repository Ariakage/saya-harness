import { CheckCircle2, Circle, Cpu, Layers, Plug, Sparkles } from "lucide-react";
import type {
  BootstrapStateDto_Serialize,
  ContextEntryDto,
  McpStatusBadgeDto,
  SessionItemDto_Serialize,
  TodoItemDto,
  WorkModeDto,
} from "../bindings";
import { WORK_MODE_STYLES, formatTokenCount } from "../lib/sanitize";

export interface ContextSidebarProps {
  bootstrap: BootstrapStateDto_Serialize | null;
  workMode: WorkModeDto;
  contextEntries: ContextEntryDto[];
  contextSkillsCount: number | null;
  contextEstimated: boolean;
  todos: TodoItemDto[];
  mcpServers: McpStatusBadgeDto[];
  recentSessions: SessionItemDto_Serialize[];
  activeSessionId: string | null;
}

const BAR_COLORS = [
  "#58A6FF",
  "#7DCB9B",
  "#BB9AF7",
  "#F6AD55",
  "#38BDF8",
  "#34D399",
  "#F472B6",
  "#A78BFA",
  "#94A3B8",
];

export function ContextSidebar({
  bootstrap,
  workMode,
  contextEntries,
  contextSkillsCount,
  contextEstimated,
  todos,
  mcpServers,
  recentSessions,
  activeSessionId,
}: ContextSidebarProps) {
  const modeStyle = WORK_MODE_STYLES[workMode];
  const windowTokens = bootstrap?.context_window_tokens ?? null;
  const totalContextTokens = contextEntries.reduce((acc, item) => acc + item.tokens, 0);
  const usagePercent =
    windowTokens && windowTokens > 0
      ? Math.min(100, Math.round((totalContextTokens / windowTokens) * 100))
      : 0;

  return (
    <aside
      className="flex w-72 shrink-0 flex-col gap-4 overflow-y-auto border-l border-[#303038] bg-[#141416] p-3 text-xs"
      data-testid="context-sidebar"
    >
      <section className="space-y-2 rounded-md border border-[#303038] bg-[#09090B] p-2.5">
        <div className="flex items-center justify-between">
          <span className="inline-flex items-center gap-1.5 font-medium text-[#E8E8ED]">
            <Cpu className="h-3.5 w-3.5 text-[#58A6FF]" />
            Runtime
          </span>
          <span
            className={`rounded border px-1.5 py-0.5 text-[10px] font-semibold ${modeStyle.badgeClass}`}
          >
            {modeStyle.label}
          </span>
        </div>
        <div className="space-y-1 text-[11px] text-[#81818B]">
          <div className="flex justify-between gap-2">
            <span>Profile</span>
            <span className="truncate font-mono text-[#D4D4D8]">
              {bootstrap?.selected_profile ?? "unset"}
            </span>
          </div>
          <div className="flex justify-between gap-2">
            <span>Model</span>
            <span className="truncate font-mono text-[#D4D4D8]">
              {bootstrap?.selected_model ?? "unset"}
            </span>
          </div>
          <div className="flex justify-between gap-2">
            <span>Sandbox</span>
            <span className="truncate font-mono text-[#D4D4D8]">
              {bootstrap?.sandbox_backend ?? "default"}
            </span>
          </div>
          <div className="truncate pt-0.5 text-[10px]" title={bootstrap?.workspace}>
            {bootstrap?.workspace ?? ""}
          </div>
        </div>
      </section>

      <section className="space-y-2 rounded-md border border-[#303038] bg-[#09090B] p-2.5">
        <div className="flex items-center justify-between">
          <span className="inline-flex items-center gap-1.5 font-medium text-[#E8E8ED]">
            <Layers className="h-3.5 w-3.5 text-[#7DCB9B]" />
            Context
          </span>
          <span className="font-mono text-[11px] text-[#81818B]">
            {contextEstimated ? "~" : ""}
            {formatTokenCount(totalContextTokens)}
            {windowTokens ? ` / ${formatTokenCount(windowTokens)} (${usagePercent}%)` : ""}
          </span>
        </div>

        {contextEntries.length === 0 ? (
          <div className="text-[11px] text-[#81818B]">
            Context breakdown updates on model turns.
          </div>
        ) : (
          <div className="space-y-1.5">
            <div className="flex h-2 w-full overflow-hidden rounded bg-[#202024]">
              {contextEntries.map((entry, idx) => {
                const denom = windowTokens && windowTokens > 0 ? windowTokens : Math.max(1, totalContextTokens);
                const widthPct = Math.max(2, Math.round((entry.tokens / denom) * 100));
                return (
                  <div
                    key={entry.label}
                    style={{
                      width: `${widthPct}%`,
                      backgroundColor: BAR_COLORS[idx % BAR_COLORS.length],
                    }}
                    title={`${entry.label}: ${entry.tokens} tokens`}
                  />
                );
              })}
            </div>
            <ul className="space-y-1">
              {contextEntries.map((entry, idx) => (
                <li
                  key={entry.label}
                  className="flex items-center justify-between text-[11px]"
                >
                  <span className="inline-flex items-center gap-1.5 text-[#D4D4D8]">
                    <span
                      className="h-2 w-2 rounded-full"
                      style={{
                        backgroundColor: BAR_COLORS[idx % BAR_COLORS.length],
                      }}
                    />
                    {entry.label}
                  </span>
                  <span className="font-mono text-[#81818B]">
                    {formatTokenCount(entry.tokens)}
                  </span>
                </li>
              ))}
            </ul>
            {contextSkillsCount != null && (
              <div className="inline-flex items-center gap-1 pt-0.5 text-[10px] text-[#81818B]">
                <Sparkles className="h-3 w-3 text-[#BB9AF7]" />
                {contextSkillsCount} active skill(s)
              </div>
            )}
          </div>
        )}
      </section>

      <section className="space-y-2 rounded-md border border-[#303038] bg-[#09090B] p-2.5">
        <div className="flex items-center justify-between">
          <span className="font-medium text-[#E8E8ED]">Tasks</span>
          <span className="font-mono text-[11px] text-[#81818B]">
            {todos.filter((t) => t.status === "completed").length}/{todos.length}
          </span>
        </div>
        {todos.length === 0 ? (
          <div className="text-[11px] text-[#81818B]">No active tasks.</div>
        ) : (
          <ul className="space-y-1.5">
            {todos.map((item) => (
              <li key={item.id} className="flex items-start gap-1.5 text-[11px]">
                {item.status === "completed" ? (
                  <CheckCircle2 className="mt-0.5 h-3.5 w-3.5 shrink-0 text-[#7DCB9B]" />
                ) : item.status === "in_progress" ? (
                  <span className="mt-1.5 h-2 w-2 shrink-0 animate-pulse rounded-full bg-[#F6AD55]" />
                ) : (
                  <Circle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-[#81818B]" />
                )}
                <span
                  className={
                    item.status === "completed"
                      ? "text-[#81818B] line-through"
                      : "text-[#D4D4D8]"
                  }
                >
                  {item.content}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="space-y-2 rounded-md border border-[#303038] bg-[#09090B] p-2.5">
        <div className="flex items-center justify-between">
          <span className="inline-flex items-center gap-1.5 font-medium text-[#E8E8ED]">
            <Plug className="h-3.5 w-3.5 text-[#58A6FF]" />
            MCP
          </span>
          <span className="font-mono text-[11px] text-[#81818B]">
            {mcpServers.length}
          </span>
        </div>
        {mcpServers.length === 0 ? (
          <div className="text-[11px] text-[#81818B]">No active MCP badges.</div>
        ) : (
          <ul className="space-y-1">
            {mcpServers.map((srv) => (
              <li
                key={srv.server}
                className="flex items-center justify-between gap-2 text-[11px]"
              >
                <span className="truncate text-[#D4D4D8]">{srv.server}</span>
                <span className="shrink-0 font-mono text-[10px] text-[#81818B]">
                  {srv.state} · {srv.tools}t
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="space-y-2 rounded-md border border-[#303038] bg-[#09090B] p-2.5">
        <div className="flex items-center justify-between">
          <span className="font-medium text-[#E8E8ED]">Recent Sessions</span>
          <span className="font-mono text-[11px] text-[#81818B]">
            {recentSessions.length}
          </span>
        </div>
        {recentSessions.length === 0 ? (
          <div className="text-[11px] text-[#81818B]">No saved sessions.</div>
        ) : (
          <ul className="space-y-1.5">
            {recentSessions.slice(0, 6).map((item) => {
              const isCurrent = item.id === activeSessionId;
              return (
                <li
                  key={item.id}
                  className={`rounded border px-2 py-1.5 text-[11px] ${
                    isCurrent
                      ? "border-[#58A6FF]/50 bg-[#58A6FF]/10"
                      : "border-[#303038] bg-[#141416]"
                  }`}
                >
                  <div className="truncate font-medium text-[#E8E8ED]">
                    {item.title ?? "Untitled session"}
                  </div>
                  <div className="mt-0.5 truncate text-[10px] text-[#81818B]">
                    {item.work_mode.toUpperCase()} · {item.profile}
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </section>
    </aside>
  );
}
