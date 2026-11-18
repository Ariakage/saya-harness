import type { HTMLAttributes } from "react";

interface SquaresLoaderProps extends HTMLAttributes<HTMLSpanElement> {
  /** Square grid pixel size (defaults to 14px for inline UI controls). */
  size?: number;
  /** Accessible label when standalone; omit for decorative indicators paired with text. */
  label?: string;
}

/**
 * `Squares · 02 — Loading` 2x2 sliding squares indicator.
 * Pure HTML/CSS div geometry so packaging security checks never see inline SVG.
 */
export function SquaresLoader({
  size = 14,
  label,
  className = "",
  style,
  ...rest
}: SquaresLoaderProps) {
  const isDecorative = !label;
  return (
    <span
      role={isDecorative ? undefined : "status"}
      aria-label={label}
      aria-hidden={isDecorative ? true : undefined}
      className={`soyo-squares-loader ${className}`.trim()}
      style={{
        width: `${size}px`,
        height: `${size}px`,
        ...style,
      }}
      {...rest}
    >
      <span className="soyo-squares-loader__sq is-a" />
      <span className="soyo-squares-loader__sq is-b" />
      <span className="soyo-squares-loader__sq is-c" />
    </span>
  );
}
