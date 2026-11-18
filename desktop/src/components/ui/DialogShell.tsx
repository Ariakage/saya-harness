import { type ReactNode, useEffect } from "react";

export type DialogSize = "sm" | "md" | "lg";

const DIALOG_WIDTH_CLASSES: Record<DialogSize, string> = {
  sm: "max-w-[480px]",
  md: "max-w-[640px]",
  lg: "max-w-[760px]",
};

export interface DialogShellProps {
  open: boolean;
  onClose?: () => void;
  ariaLabel: string;
  testId?: string;
  size?: DialogSize;
  panelClassName?: string;
  children: ReactNode;
}

export function DialogShell({
  open,
  onClose,
  ariaLabel,
  testId,
  size = "md",
  panelClassName = "",
  children,
}: DialogShellProps) {
  useEffect(() => {
    if (!open || !onClose) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={ariaLabel}
      data-testid={testId}
      onMouseDown={(event) => {
        if (onClose && event.target === event.currentTarget) {
          onClose();
        }
      }}
      className="soyo-backdrop-enter fixed inset-0 z-50 flex items-center justify-center bg-[var(--scrim)] p-4 backdrop-blur-xs"
    >
      <div
        className={`soyo-dialog-enter w-full overflow-hidden rounded-[var(--radius-xl)] border border-[var(--border)] bg-[var(--surface-overlay)] text-[var(--text-primary)] shadow-[var(--shadow-dialog)] ${DIALOG_WIDTH_CLASSES[size]} ${panelClassName}`.trim()}
      >
        {children}
      </div>
    </div>
  );
}
