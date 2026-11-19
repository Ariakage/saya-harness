import { check, type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";
import { Download, RefreshCw, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { getMessages } from "../lib/i18n";
import { sanitizeDisplayText } from "../lib/sanitize";
import { isBrowserPreview } from "../preview/browserPreview";
import { useAppStore } from "../store/useAppStore";
import { Button } from "./ui/Button";
import { SquaresLoader } from "./SquaresLoader";

export function UpdaterBanner() {
  const locale = useAppStore((s) => s.locale);
  const t = getMessages(locale);
  const [update, setUpdate] = useState<Update | null>(null);
  const [installing, setInstalling] = useState(false);
  const [installed, setInstalled] = useState(false);
  const [progressText, setProgressText] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const installedRef = useRef(false);

  useEffect(() => {
    if (isBrowserPreview()) {
      return;
    }

    let cancelled = false;
    let discovered: Update | null = null;

    void (async () => {
      try {
        const candidate = await check();
        if (cancelled) {
          if (candidate) {
            await candidate.close().catch(() => {});
          }
          return;
        }
        discovered = candidate;
        if (candidate) {
          setUpdate(candidate);
        }
      } catch {
        // No-op when updater pubkey is unconfigured in dev or offline.
      }
    })();

    return () => {
      cancelled = true;
      if (discovered && !installedRef.current) {
        void discovered.close().catch(() => {});
      }
    };
  }, []);

  if (!update) {
    return null;
  }

  const handleInstall = async () => {
    if (installing || installed) {
      return;
    }
    setInstalling(true);
    setErrorMsg(null);
    let totalBytes = 0;
    let downloaded = 0;

    try {
      const msg = getMessages(useAppStore.getState().locale);
      await update.downloadAndInstall((event: DownloadEvent) => {
        if (event.event === "Started") {
          totalBytes = event.data.contentLength ?? 0;
          setProgressText(
            totalBytes > 0
              ? `0 / ${Math.max(1, Math.round(totalBytes / 1024))} KiB`
              : msg.updater.downloading
          );
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          setProgressText(
            totalBytes > 0
              ? `${Math.min(100, Math.round((downloaded / totalBytes) * 100))}%`
              : `${Math.max(1, Math.round(downloaded / 1024))} KiB`
          );
        } else if (event.event === "Finished") {
          setProgressText(msg.updater.downloadedInstalling);
        }
      });
      installedRef.current = true;
      setInstalled(true);
      setProgressText(msg.updater.installedRestart);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setErrorMsg(sanitizeDisplayText(msg));
    } finally {
      setInstalling(false);
    }
  };

  const handleDismiss = async () => {
    const target = update;
    setUpdate(null);
    if (!installedRef.current) {
      await target.close().catch(() => {});
    }
  };

  return (
    <div
      data-testid="updater-banner"
      className="soyo-panel-enter flex flex-wrap items-center justify-between gap-2 border-b border-[var(--border)] bg-[var(--surface-raised)] px-4 py-1.5 text-[12px] text-[var(--text-primary)]"
    >
      <div className="flex min-w-0 items-center gap-2">
        <Download className="h-3.5 w-3.5 shrink-0 text-[var(--accent-text)]" />
        <span className="font-medium">
          {t.updater.available(sanitizeDisplayText(update.version))}
        </span>
        <span className="font-mono text-[11px] text-[var(--text-tertiary)]">
          {t.updater.current(sanitizeDisplayText(update.currentVersion))}
        </span>
        {progressText && (
          <span className="font-mono text-[11px] text-[var(--success)]">
            {progressText}
          </span>
        )}
        {errorMsg && (
          <span className="text-[11px] text-[var(--danger)]">{errorMsg}</span>
        )}
      </div>

      <div className="flex items-center gap-2">
        {!installed && (
          <Button
            variant="primary"
            size="sm"
            disabled={installing}
            onClick={() => void handleInstall()}
          >
            {installing ? (
              <SquaresLoader size={12} />
            ) : (
              <RefreshCw className="h-3 w-3" strokeWidth={1.75} aria-hidden="true" />
            )}
            {installing ? t.updater.installingBtn : t.updater.installRestartBtn}
          </Button>
        )}
        <button
          type="button"
          onClick={() => void handleDismiss()}
          aria-label={t.a11y.dismissUpdater}
          className="rounded-[var(--radius-sm)] p-0.5 text-[var(--text-tertiary)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      </div>
    </div>
  );
}
