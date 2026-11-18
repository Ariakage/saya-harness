import { type ButtonHTMLAttributes, forwardRef } from "react";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";
export type ButtonSize = "sm" | "md" | "icon";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
}

const VARIANT_CLASSES: Record<ButtonVariant, string> = {
  primary:
    "border border-transparent bg-[var(--accent)] text-[var(--on-accent)] hover:bg-[var(--accent-hover)] active:bg-[var(--accent-pressed)]",
  secondary:
    "border border-[var(--border-strong)] bg-[var(--surface-raised)] text-[var(--text-primary)] hover:bg-[var(--surface-hover)]",
  ghost:
    "border border-transparent bg-transparent text-[var(--text-secondary)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]",
  danger:
    "border border-[var(--danger)] bg-[var(--danger-surface)] text-[var(--danger)] hover:bg-[var(--surface-hover)]",
};

const SIZE_CLASSES: Record<ButtonSize, string> = {
  sm: "h-7 px-3 text-[12px] leading-[18px] rounded-[var(--radius-md)]",
  md: "h-8 px-3.5 text-[13px] leading-[20px] rounded-[var(--radius-md)]",
  icon: "h-7 w-7 p-0 rounded-[var(--radius-md)]",
};

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(
  function Button(
    {
      variant = "secondary",
      size = "sm",
      type = "button",
      className = "",
      children,
      ...rest
    },
    ref
  ) {
    return (
      <button
        ref={ref}
        type={type}
        className={`inline-flex shrink-0 items-center justify-center gap-1.5 font-medium whitespace-nowrap transition-colors duration-150 disabled:cursor-not-allowed disabled:opacity-40 ${VARIANT_CLASSES[variant]} ${SIZE_CLASSES[size]} ${className}`.trim()}
        {...rest}
      >
        {children}
      </button>
    );
  }
);
