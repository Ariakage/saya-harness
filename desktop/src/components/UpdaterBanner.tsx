import { check, type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";
import { Download, RefreshCw, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { sanitizeDisplayText } from "../lib/sanitize";
import { isBrowserPreview } from "../preview/browserPreview";

export function UpdaterBanner() {
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
      await update.downloadAndInstall((event: DownloadEvent) => {
        if (event.event === "Started") {
          totalBytes = event.data.contentLength ?? 0;
          setProgressText(
            totalBytes > 0
              ? `0 / ${Math.max(1, Math.round(totalBytes / 1024))} KiB`
              : "Downloading..."
          );
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          setProgressText(
            totalBytes > 0
              ? `${Math.min(100, Math.round((downloaded / totalBytes) * 100))}%`
              : `${Math.max(1, Math.round(downloaded / 1024))} KiB`
          );
        } else if (event.event === "Finished") {
          setProgressText("Downloaded; installing...");
        }
      });
      installedRef.current = true;
      setInstalled(true);
      setProgressText("Update installed — restart Soyo to finish applying.");
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
      className="flex flex-wrap items-center justify-between gap-2 border-b border-[#58A6FF]/40 bg-[#58A6FF]/10 px-4 py-1.5 text-xs text-[#E8E8ED]"
    >
      <div className="flex min-w-0 items-center gap-2">
        <Download className="h-3.5 w-3.5 shrink-0 text-[#58A6FF]" />
        <span className="font-medium">
          Update available: v{sanitizeDisplayText(update.version)}
        </span>
        <span className="font-mono text-[11px] text-[#81818B]">
          (current v{sanitizeDisplayText(update.currentVersion)})
        </span>
        {progressText && (
          <span className="font-mono text-[11px] text-[#7DCB9B]">
            {progressText}
          </span>
        )}
        {errorMsg && (
          <span className="text-[11px] text-[#F87171]">{errorMsg}</span>
        )}
      </div>

      <div className="flex items-center gap-2">
        {!installed && (
          <button
            type="button"
            disabled={installing}
            onClick={() => void handleInstall()}
            className="inline-flex items-center gap-1 rounded bg-[#58A6FF] px-2.5 py-1 text-[11px] font-semibold text-[#09090B] hover:bg-[#73B5FF] disabled:opacity-50"
          >
            <RefreshCw
              className={`h-3 w-3 ${installing ? "animate-spin" : ""}`}
            />
            {installing ? "Installing..." : "Install & Restart"}
          </button>
        )}
        <button
          type="button"
          onClick={() => void handleDismiss()}
          aria-label="Dismiss update banner"
          className="rounded p-0.5 text-[#81818B] hover:bg-[#202024] hover:text-[#E8E8ED]"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      </div>
    </div>
  );
}
