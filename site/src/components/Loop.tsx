import { MoveRight, Repeat } from "lucide-react";
import { useI18n } from "@/lib/i18n";

const STEPS = [
  "loop.step.model",
  "loop.step.tools",
  "loop.step.results",
  "loop.step.repeat",
] as const;

/* The agent loop as a one-line statement + flow — the window above shows it. */
export function Loop() {
  const { t } = useI18n();

  return (
    <section id="loop" className="border-t border-border-subtle">
      <div className="mx-auto max-w-[1040px] px-5 py-20 sm:py-24">
        <h2 className="font-display max-w-[720px] text-[30px] font-semibold leading-[1.2] tracking-[-0.01em] text-text-primary">
          {t("loop.title")}
        </h2>
        <p className="mt-4 max-w-[560px] text-[14px] leading-[1.7] text-text-secondary">
          {t("loop.body")}
        </p>

        <div className="mt-8 flex flex-wrap items-center gap-x-3 gap-y-2">
          {STEPS.map((s, i) => (
            <span key={s} className="flex items-center gap-3">
              {i > 0 && (
                <MoveRight size={13} strokeWidth={1.75} className="text-text-tertiary" />
              )}
              <code className="rounded-[6px] border border-border bg-surface-raised px-2.5 py-1.5 text-[12px] text-text-primary">
                {t(s)}
              </code>
            </span>
          ))}
          <span className="flex items-center gap-3">
            <MoveRight size={13} strokeWidth={1.75} className="text-text-tertiary" />
            <code className="flex h-[30px] w-[30px] items-center justify-center rounded-[6px] border border-dashed border-border-strong text-accent-text">
              <Repeat size={13} strokeWidth={1.75} />
            </code>
          </span>
        </div>

        <p className="mt-8 max-w-[560px] border-l-2 border-accent pl-4 text-[12.5px] leading-[1.65] text-text-tertiary">
          {t("loop.caption")}
        </p>
      </div>
    </section>
  );
}
