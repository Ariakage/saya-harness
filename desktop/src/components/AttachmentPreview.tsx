import { FileText, Image as ImageIcon, Paperclip, X } from "lucide-react";
import type { StagedAttachmentDto } from "../bindings";
import { getMessages } from "../lib/i18n";
import { sanitizeDisplayText } from "../lib/sanitize";
import { useAppStore } from "../store/useAppStore";

export interface StagedAttachmentsStripProps {
  items: StagedAttachmentDto[];
  onRemove: (id: string) => void;
}

export function StagedAttachmentsStrip({
  items,
  onRemove,
}: StagedAttachmentsStripProps) {
  const locale = useAppStore((s) => s.locale);
  const t = getMessages(locale);

  if (items.length === 0) {
    return null;
  }

  return (
    <div
      className="flex flex-wrap gap-2 pb-1.5"
      data-testid="staged-attachments-strip"
    >
      {items.map((item) => {
        const isImage = item.kind === "image";
        return (
          <div
            key={item.id}
            className="soyo-panel-enter flex items-center gap-2 rounded-[var(--radius-md)] border border-[var(--border)] bg-[var(--surface-raised)] px-[9px] py-[6px] text-[12px] text-[var(--text-secondary)]"
          >
            {isImage ? (
              <img
                src={item.preview_url}
                alt={sanitizeDisplayText(item.filename)}
                className="h-[26px] w-[26px] rounded-[var(--radius-md)] object-cover"
              />
            ) : (
              <FileText className="h-3.5 w-3.5 shrink-0 text-[var(--text-tertiary)]" />
            )}
            <div className="flex items-baseline gap-1.5 max-w-[200px]">
              <span className="truncate font-medium text-[var(--text-primary)]">
                {sanitizeDisplayText(item.filename)}
              </span>
              <span className="shrink-0 font-mono text-[11px] text-[var(--text-tertiary)]">
                {Math.max(1, Math.round(item.size_bytes / 1024))} KB
              </span>
            </div>
            <button
              type="button"
              onClick={() => onRemove(item.id)}
              aria-label={t.a11y.removeAttachment(item.filename)}
              className="rounded p-0.5 text-[var(--text-tertiary)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
            >
              <X className="h-3 w-3" />
            </button>
          </div>
        );
      })}
    </div>
  );
}

export interface SessionMediaPreviewProps {
  sessionId: string | null;
  mediaRefs: string[];
}

export function buildMediaAttachmentUrl(sessionId: string, reference: string): string {
  return `soyo-attachment://localhost/media/${encodeURIComponent(sessionId)}?ref=${encodeURIComponent(reference)}`;
}

export function SessionMediaPreview({
  sessionId,
  mediaRefs,
}: SessionMediaPreviewProps) {
  const locale = useAppStore((s) => s.locale);
  const t = getMessages(locale);

  if (!sessionId || mediaRefs.length === 0) {
    return null;
  }

  return (
    <div className="space-y-1.5" data-testid="session-media-preview">
      <div className="text-[11px] uppercase tracking-wider text-[var(--text-tertiary)]">
        {t.cards.attachmentsHeader(mediaRefs.length)}
      </div>
      <div className="flex flex-wrap gap-2">
        {mediaRefs.map((ref) => {
          const url = buildMediaAttachmentUrl(sessionId, ref);
          const likelyImage =
            /\.(png|jpe?g|webp|gif)$/i.test(ref) || ref.includes("image");
          return (
            <div
              key={ref}
              className="flex items-center gap-2 rounded-[var(--radius-md)] border border-[var(--border)] bg-[var(--surface-raised)] p-1.5 text-[11px]"
            >
              {likelyImage ? (
                <img
                  src={url}
                  alt={ref}
                  className="h-12 w-12 rounded-[var(--radius-md)] border border-[var(--border)] object-cover"
                />
              ) : (
                <Paperclip className="h-4 w-4 shrink-0 text-[var(--accent-text)]" />
              )}
              <div className="max-w-[200px]">
                <div className="flex items-center gap-1 truncate font-mono text-[11px] text-[var(--text-primary)]">
                  {likelyImage && (
                    <ImageIcon className="h-3 w-3 shrink-0 text-[var(--success)]" />
                  )}
                  <span className="truncate">{sanitizeDisplayText(ref)}</span>
                </div>
                <div className="truncate font-mono text-[11px] text-[var(--text-tertiary)]">
                  {url}
                </div>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
