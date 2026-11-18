import { Code2, Eye, History, Plus, Sparkles, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { ApprovalDialog } from "./components/ApprovalDialog";
import { ClarificationDialog } from "./components/ClarificationDialog";
import { Composer } from "./components/Composer";
import { ContextSidebar } from "./components/ContextSidebar";
import { MarkdownRenderer } from "./components/MarkdownRenderer";
import { ModeAndPermissionBar } from "./components/ModeAndPermissionBar";
import { SessionBrowserDialog } from "./components/SessionBrowserDialog";
import { ThinkingCardView } from "./components/ThinkingCardView";
import { ToolCardView } from "./components/ToolCardView";
import { WORK_MODE_STYLES } from "./lib/sanitize";
import {
  PREVIEW_APPROVAL_REQUEST,
  PREVIEW_INPUT_REQUEST,
  type PreviewScene,
  getPreviewScene,
  isBrowserPreview,
  setPreviewScene,
} from "./preview/browserPreview";
import { useAppStore } from "./store/useAppStore";

export function App() {
  const bootstrap = useAppStore((s) => s.bootstrap);
  const loading = useAppStore((s) => s.loading);
  const sessionId = useAppStore((s) => s.sessionId);
  const sessionTitle = useAppStore((s) => s.sessionTitle);
  const historyOmitted = useAppStore((s) => s.historyOmitted);
  const workMode = useAppStore((s) => s.workMode);
  const pendingMode = useAppStore((s) => s.pendingMode);
  const permissionMode = useAppStore((s) => s.permissionMode);
  const messages = useAppStore((s) => s.messages);
  const thinkingEntries = useAppStore((s) => s.thinkingEntries);
  const contextEntries = useAppStore((s) => s.contextEntries);
  const contextSkillsCount = useAppStore((s) => s.contextSkillsCount);
  const contextEstimated = useAppStore((s) => s.contextEstimated);
  const mcpServers = useAppStore((s) => s.mcpServers);
  const todos = useAppStore((s) => s.todos);
  const recentSessions = useAppStore((s) => s.recentSessions);
  const runActive = useAppStore((s) => s.runActive);
  const cancelling = useAppStore((s) => s.cancelling);
  const summaryLine = useAppStore((s) => s.summaryLine);
  const markdownMode = useAppStore((s) => s.markdownMode);
  const pendingApproval = useAppStore((s) => s.pendingApproval);
  const pendingInput = useAppStore((s) => s.pendingInput);
  const configChangeSessionId = useAppStore((s) => s.configChangeSessionId);
  const recoveryBanner = useAppStore((s) => s.recoveryBanner);
  const error = useAppStore((s) => s.error);

  const initApp = useAppStore((s) => s.initApp);
  const startNewSession = useAppStore((s) => s.startNewSession);
  const submitPrompt = useAppStore((s) => s.submitPrompt);
  const cancelActiveRun = useAppStore((s) => s.cancelActiveRun);
  const compactCurrentSession = useAppStore((s) => s.compactCurrentSession);
  const replyApproval = useAppStore((s) => s.replyApproval);
  const replyInput = useAppStore((s) => s.replyInput);
  const setWorkMode = useAppStore((s) => s.setWorkMode);
  const cycleWorkMode = useAppStore((s) => s.cycleWorkMode);
  const setPermissionMode = useAppStore((s) => s.setPermissionMode);
  const resumeSession = useAppStore((s) => s.resumeSession);
  const clearConfigChangePrompt = useAppStore((s) => s.clearConfigChangePrompt);
  const toggleMarkdownMode = useAppStore((s) => s.toggleMarkdownMode);
  const clearError = useAppStore((s) => s.clearError);

  const previewMode = isBrowserPreview();
  const [scene, setScene] = useState<PreviewScene>(() => getPreviewScene());
  const [sessionsOpen, setSessionsOpen] = useState(false);
  const scrollRef = useRef<HTMLDivElement | null>(null);

  const applyPreviewOverlay = (targetScene: PreviewScene) => {
    if (!previewMode) return;
    useAppStore.setState({
      pendingApproval:
        targetScene === "approval" ? PREVIEW_APPROVAL_REQUEST : null,
      pendingInput:
        targetScene === "clarification" ? PREVIEW_INPUT_REQUEST : null,
    });
  };

  useEffect(() => {
    void initApp().then(() => {
      applyPreviewOverlay(getPreviewScene());
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initApp]);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) {
      el.scrollTop = el.scrollHeight;
    }
  }, [messages.length, thinkingEntries.length]);

  const handleSceneChange = (nextScene: PreviewScene) => {
    setPreviewScene(nextScene);
    setScene(nextScene);
    void initApp().then(() => {
      applyPreviewOverlay(nextScene);
    });
  };

  const modeStyle = WORK_MODE_STYLES[workMode];

  return (
    <div className="flex h-screen w-screen flex-col bg-[#09090B] text-[#D4D4D8]">
      <header className="flex flex-wrap items-center justify-between gap-3 border-b border-[#303038] bg-[#141416] px-4 py-2">
        <div className="flex min-w-0 items-center gap-2.5">
          <img
            src="/app-icon.png"
            alt="Soyo"
            className="h-6 w-6 shrink-0 rounded-full border border-[#303038] object-cover"
          />
          <h1 className="shrink-0 text-sm font-semibold tracking-tight text-[#E8E8ED]">
            Soyo Harness
          </h1>
          <ModeAndPermissionBar
            workMode={workMode}
            pendingMode={pendingMode}
            permissionMode={permissionMode}
            runActive={runActive}
            onSelectMode={setWorkMode}
            onSelectPermission={setPermissionMode}
          />
          <span className="truncate text-xs text-[#81818B]">{sessionTitle}</span>
        </div>

        <div className="flex shrink-0 items-center gap-2">
          {previewMode && (
            <div className="flex items-center gap-1 rounded-md border border-[#58A6FF]/40 bg-[#58A6FF]/10 px-2 py-1 text-[11px]">
              <Sparkles className="h-3 w-3 text-[#58A6FF]" />
              <span className="mr-1 text-[#58A6FF]">Preview</span>
              {(["conversation", "markdown", "approval", "clarification"] as const).map(
                (item) => (
                  <button
                    key={item}
                    type="button"
                    onClick={() => handleSceneChange(item)}
                    className={`rounded px-1.5 py-0.5 text-[10px] ${
                      scene === item
                        ? "bg-[#58A6FF] font-semibold text-[#09090B]"
                        : "text-[#D4D4D8] hover:bg-[#202024]"
                    }`}
                  >
                    {item}
                  </button>
                )
              )}
            </div>
          )}

          <button
            type="button"
            onClick={() => setSessionsOpen(true)}
            className="inline-flex items-center gap-1 rounded-md border border-[#303038] bg-[#202024] px-2.5 py-1 text-[11px] text-[#D4D4D8] hover:border-[#58A6FF]"
          >
            <History className="h-3 w-3" />
            Sessions
          </button>

          <button
            type="button"
            onClick={toggleMarkdownMode}
            title="Toggle global Markdown Render / Source mode"
            className="inline-flex items-center gap-1 rounded-md border border-[#303038] bg-[#202024] px-2.5 py-1 text-[11px] text-[#D4D4D8] hover:border-[#58A6FF]"
          >
            {markdownMode === "render" ? (
              <>
                <Code2 className="h-3 w-3" />
                Source
              </>
            ) : (
              <>
                <Eye className="h-3 w-3" />
                Render
              </>
            )}
          </button>

          <button
            type="button"
            onClick={() => void startNewSession()}
            disabled={runActive}
            className="inline-flex items-center gap-1 rounded-md border border-[#303038] bg-[#202024] px-2.5 py-1 text-[11px] text-[#E8E8ED] hover:border-[#58A6FF] disabled:opacity-40"
          >
            <Plus className="h-3 w-3" />
            New Session
          </button>
        </div>
      </header>

      <div className="flex min-h-0 flex-1 overflow-hidden">
        <div className="flex min-w-0 flex-1 flex-col">
          {error && (
            <div className="flex items-center justify-between gap-2 border-b border-[#F87171]/40 bg-[#F87171]/10 px-4 py-2 text-xs text-[#F87171]">
              <span>
                <strong>{error.code}:</strong> {error.message}
              </span>
              <button
                type="button"
                onClick={clearError}
                aria-label="Dismiss error"
                className="rounded p-0.5 hover:bg-[#F87171]/20"
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </div>
          )}

          {bootstrap && !bootstrap.config_ready && (
            <div className="border-b border-[#F6AD55]/40 bg-[#F6AD55]/10 px-4 py-2 text-xs text-[#F6AD55]">
              Configuration incomplete ({bootstrap.config_path}):{" "}
              {bootstrap.config_error ?? "Select or configure a profile first."}
            </div>
          )}

          {recoveryBanner && (
            <div className="border-b border-[#58A6FF]/40 bg-[#58A6FF]/10 px-4 py-1.5 text-xs text-[#58A6FF]">
              {recoveryBanner}
            </div>
          )}

          <div ref={scrollRef} className="flex-1 overflow-y-auto px-4 py-4">
            <div className="mx-auto flex max-w-4xl flex-col gap-3">
              {historyOmitted && (
                <div className="rounded-md border border-[#303038] bg-[#141416] px-3 py-1.5 text-[11px] text-[#81818B]">
                  Earlier history omitted to stay within desktop restore window; full history remains on disk.
                </div>
              )}

              {loading && messages.length === 0 ? (
                <div className="py-16 text-center text-xs text-[#81818B]">
                  Loading runtime state...
                </div>
              ) : messages.length === 0 && thinkingEntries.length === 0 ? (
                <div className="rounded-xl border border-[#303038] bg-[#141416] p-8 text-center">
                  <div className="text-sm font-medium text-[#E8E8ED]">
                    Ready in {modeStyle.label} mode
                  </div>
                  <p className="mt-1 text-xs text-[#81818B]">
                    Send a prompt below to start a schema v1 run with in-process `soyo-app`.
                  </p>
                </div>
              ) : null}

              {messages.map((msg) => {
                if (msg.role === "user") {
                  return (
                    <div
                      key={msg.id}
                      className="self-end rounded-xl border border-[#58A6FF]/30 bg-[#202024] px-3.5 py-2.5 text-sm text-[#E8E8ED] max-w-2xl"
                    >
                      <div className="mb-1 text-[10px] font-medium uppercase tracking-wider text-[#58A6FF]">
                        {msg.label}
                      </div>
                      <pre className="whitespace-pre-wrap font-sans text-sm leading-relaxed">
                        {msg.text}
                      </pre>
                    </div>
                  );
                }

                if (msg.role === "tool") {
                  return <ToolCardView key={msg.id} message={msg} />;
                }

                if (msg.role === "run_summary") {
                  return (
                    <div
                      key={msg.id}
                      className="flex items-center justify-between rounded-md border border-[#303038] bg-[#141416]/70 px-3 py-1.5 text-[11px] text-[#81818B]"
                    >
                      <span className="font-mono uppercase text-[#D4D4D8]">
                        {msg.label}
                      </span>
                      <span className="font-mono">{msg.text}</span>
                    </div>
                  );
                }

                if (msg.role === "system") {
                  return (
                    <div
                      key={msg.id}
                      className="rounded-md border border-[#303038] bg-[#141416] px-3 py-2 text-xs text-[#81818B]"
                    >
                      {msg.text}
                    </div>
                  );
                }

                return (
                  <article
                    key={msg.id}
                    className="rounded-xl border border-[#303038] bg-[#141416] px-4 py-3"
                  >
                    <div className="mb-1.5 flex items-center justify-between text-[11px] text-[#81818B]">
                      <span className="font-medium text-[#E8E8ED]">
                        {msg.label}
                      </span>
                      {msg.streaming && (
                        <span className="inline-flex items-center gap-1 text-[10px] text-[#58A6FF]">
                          <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-[#58A6FF]" />
                          streaming
                        </span>
                      )}
                    </div>
                    <MarkdownRenderer
                      content={msg.text}
                      mode={markdownMode}
                      streaming={msg.streaming}
                    />
                  </article>
                );
              })}

              {thinkingEntries.length > 0 && (
                <ThinkingCardView entries={thinkingEntries} />
              )}

              {summaryLine && (
                <div className="text-right font-mono text-[11px] text-[#81818B]">
                  {summaryLine}
                </div>
              )}
            </div>
          </div>

          <Composer
            disabled={!bootstrap?.config_ready && !previewMode}
            runActive={runActive}
            cancelling={cancelling}
            hasSession={sessionId !== null}
            onSubmit={(prompt) => submitPrompt(prompt)}
            onCancel={cancelActiveRun}
            onCompact={compactCurrentSession}
            onCycleMode={cycleWorkMode}
          />
        </div>

        <ContextSidebar
          bootstrap={bootstrap}
          workMode={workMode}
          contextEntries={contextEntries}
          contextSkillsCount={contextSkillsCount}
          contextEstimated={contextEstimated}
          todos={todos}
          mcpServers={mcpServers}
          recentSessions={recentSessions}
          activeSessionId={sessionId}
        />
      </div>

      {pendingApproval && (
        <ApprovalDialog request={pendingApproval} onReply={replyApproval} />
      )}
      {!pendingApproval && pendingInput && (
        <ClarificationDialog request={pendingInput} onReply={replyInput} />
      )}
      <SessionBrowserDialog
        open={sessionsOpen}
        activeSessionId={sessionId}
        runActive={runActive}
        configChangeSessionId={configChangeSessionId}
        onClose={() => setSessionsOpen(false)}
        onResume={resumeSession}
        onClearConfigChange={clearConfigChangePrompt}
      />
    </div>
  );
}

export default App;
