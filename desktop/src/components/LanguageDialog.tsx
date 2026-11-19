import { Check, X } from "lucide-react";
import { useEffect } from "react";
import {
  LANGUAGE_OPTIONS,
  getMessages,
  type LocaleId,
} from "../lib/i18n";

export interface LanguageDialogProps {
  open: boolean;
  locale: LocaleId;
  onSelectLocale: (locale: LocaleId) => void;
  onClose: () => void;
}

export function LanguageDialog({
  open,
  locale,
  onSelectLocale,
  onClose,
}: LanguageDialogProps) {
  const t = getMessages(locale);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

  if (!open) {
    return null;
  }

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={t.languageDialog.title}
      data-testid="language-dialog"
      onClick={(e) => {
        if (e.target === e.currentTarget) {
          onClose();
        }
      }}
      className="soyo-backdrop-enter fixed inset-0 z-50 flex items-center justify-center bg-black/65 p-4 backdrop-blur-xs"
    >
      <div className="soyo-dialog-enter w-full max-w-[580px] rounded-[14px] border border-[var(--border)] bg-[var(--surface-raised)] p-5 shadow-[0_24px_70px_rgba(0,0,0,0.68)]">
        <div className="mb-4 flex items-center justify-between">
          <h2 className="text-[15px] font-semibold text-[var(--text-primary)]">
            {t.languageDialog.title}
          </h2>
          <button
            type="button"
            onClick={onClose}
            aria-label={t.languageDialog.closeAria}
            className="rounded-[7px] p-1 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div
          role="radiogroup"
          aria-label={t.languageDialog.title}
          className="grid grid-cols-1 gap-2.5 sm:grid-cols-3"
        >
          {LANGUAGE_OPTIONS.map((opt) => {
            const isSelected = opt.id === locale;
            return (
              <button
                key={opt.id}
                type="button"
                role="radio"
                aria-checked={isSelected}
                data-testid={`language-option-${opt.id}`}
                onClick={() => onSelectLocale(opt.id)}
                className={`flex flex-col rounded-[9px] border px-3 py-2.5 text-left transition-colors ${
                  isSelected
                    ? "border-[var(--border-strong)] bg-[var(--info-surface)] text-[var(--text-primary)]"
                    : "border-transparent bg-transparent text-[var(--text-primary)] hover:bg-[var(--surface-hover)]"
                }`}
              >
                <div className="flex w-full items-center justify-between gap-2">
                  <span className="truncate text-[13px] font-medium text-[var(--text-primary)]">
                    {opt.nativeLabel}
                  </span>
                  {isSelected && (
                    <Check className="h-3.5 w-3.5 shrink-0 text-[var(--info)]" />
                  )}
                </div>
                <div className="mt-0.5 truncate text-[12px] text-[var(--text-tertiary)]">
                  {opt.subtitle}
                </div>
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );
}
