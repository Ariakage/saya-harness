import { FileText, Image as ImageIcon, Paperclip, X } from "lucide-react";
import type { StagedAttachmentDto } from "../bindings";
import { sanitizeDisplayText } from "../lib/sanitize";

export interface StagedAttachmentsStripProps {
  items: StagedAttachmentDto[];
  onRemove: (id: string) => void;
}

export function StagedAttachmentsStrip({
  items,
  onRemove,
}: StagedAttachmentsStripProps) {
  if (items.length === 0) {
    return null;
  }

  return (
    <div
      className="flex flex-wrap gap-2 pb-1"
      data-testid="staged-attachments-strip"
    >
      {items.map((item) => {
        const isImage = item.kind === "image";
        return (
          <div
            key={item.id}
            className="flex items-center gap-2 rounded-md border border-[#303038] bg-[#141416] px-2 py-1 text-[11px]"
          >
            {isImage ? (
              <img
                src={item.preview_url}
                alt={sanitizeDisplayText(item.filename)}
                className="h-6 w-6 rounded object-cover"
              />
            ) : (
              <Paperclip className="h-3.5 w-3.5 text-[#58A6FF]" />
            )}
            <div className="max-w-[180px]">
              <div className="truncate font-medium text-[#E8E8ED]">
                {sanitizeDisplayText(item.filename)}
              </div>
              <div className="truncate font-mono text-[10px] text-[#81818B]">
                {item.kind} · {Math.max(1, Math.round(item.size_bytes / 1024))} KiB
              </div>
            </div>
            <button
              type="button"
              onClick={() => onRemove(item.id)}
              aria-label={`Remove ${item.filename}`}
              className="rounded p-0.5 text-[#81818B] hover:bg-[#202024] hover:text-[#E8E8ED]"
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
  if (!sessionId || mediaRefs.length === 0) {
    return null;
  }

  return (
    <div className="space-y-1.5" data-testid="session-media-preview">
      <div className="text-[10px] uppercase tracking-wider text-[#81818B]">
        Attachments ({mediaRefs.length})
      </div>
      <div className="flex flex-wrap gap-2">
        {mediaRefs.map((ref) => {
          const url = buildMediaAttachmentUrl(sessionId, ref);
          const likelyImage = /\.(png|jpe?g|webp|gif)$/i.test(ref) || ref.includes("image");
          return (
            <div
              key={ref}
              className="flex items-center gap-2 rounded border border-[#303038] bg-[#09090B] p-1.5 text-[11px]"
            >
              {likelyImage ? (
                <img
                  src={url}
                  alt={ref}
                  className="h-12 w-12 rounded border border-[#303038] object-cover"
                />
              ) : (
                <FileText className="h-4 w-4 shrink-0 text-[#58A6FF]" />
              )}
              <div className="max-w-[200px]">
                <div className="flex items-center gap-1 font-mono text-[10px] text-[#D4D4D8] truncate">
                  {likelyImage && <ImageIcon className="h-3 w-3 text-[#7DCB9B]" />}
                  <span className="truncate">{sanitizeDisplayText(ref)}</span>
                </div>
                <div className="truncate font-mono text-[9px] text-[#81818B]">
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
