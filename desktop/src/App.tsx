import { useEffect, useState } from "react";
import { commands, type DesktopError, type SessionItemDto } from "./bindings";
import { isBrowserPreview } from "./preview/browserPreview";

export function App() {
  const [sessions, setSessions] = useState<SessionItemDto[]>([]);
  const [error, setError] = useState<DesktopError | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let active = true;
    commands
      .listSessions(1)
      .then((res) => {
        if (!active) return;
        if (res.status === "ok") {
          setSessions(res.data.items);
          setError(null);
        } else {
          setError(res.error);
        }
      })
      .catch((err: unknown) => {
        if (!active) return;
        setError({
          code: "ipc_error",
          message: err instanceof Error ? err.message : String(err),
        });
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, []);

  return (
    <div className="flex h-screen w-screen flex-col bg-[#09090B] text-[#D4D4D8]">
      <header className="flex items-center justify-between border-b border-[#303038] bg-[#141416] px-4 py-2.5">
        <div className="flex items-center gap-2.5">
          <span className="h-2.5 w-2.5 rounded-full bg-[#58A6FF]" />
          <h1 className="text-sm font-semibold tracking-tight text-[#E8E8ED]">
            Soyo Harness
          </h1>
          <span className="rounded bg-[#202024] px-1.5 py-0.5 text-[11px] text-[#81818B]">
            Desktop
          </span>
          {isBrowserPreview() && (
            <span className="rounded border border-[#58A6FF]/40 bg-[#58A6FF]/15 px-1.5 py-0.5 text-[11px] text-[#58A6FF]">
              Browser Preview
            </span>
          )}
        </div>
        <div className="text-xs text-[#81818B]">
          {loading ? "Loading sessions..." : `${sessions.length} session(s)`}
        </div>
      </header>
      <main className="flex flex-1 overflow-hidden">
        <aside className="w-72 border-r border-[#303038] bg-[#141416] p-3">
          <div className="mb-2 text-xs font-medium uppercase tracking-wider text-[#81818B]">
            Recent Sessions
          </div>
          {error ? (
            <div className="rounded border border-[#F87171]/40 bg-[#F87171]/10 p-2 text-xs text-[#F87171]">
              {error.code}: {error.message}
            </div>
          ) : sessions.length === 0 && !loading ? (
            <div className="text-xs text-[#81818B]">No sessions yet.</div>
          ) : (
            <ul className="space-y-1.5">
              {sessions.map((item) => (
                <li
                  key={item.id}
                  className="rounded border border-[#303038] bg-[#09090B] px-2.5 py-2 text-xs"
                >
                  <div className="truncate font-medium text-[#E8E8ED]">
                    {item.title ?? "Untitled session"}
                  </div>
                  <div className="mt-0.5 truncate text-[11px] text-[#81818B]">
                    {item.work_mode.toUpperCase()} · {item.profile}
                  </div>
                </li>
              ))}
            </ul>
          )}
        </aside>
        <section className="flex flex-1 items-center justify-center p-6 text-sm text-[#81818B]">
          Select or start a session to begin.
        </section>
      </main>
    </div>
  );
}

export default App;
