import { useI18n } from "@/lib/i18n";

const MODES = [
  { name: "WORK", color: "var(--mode-work)", desc: "modes.work.desc", tag: "default" },
  { name: "CODE", color: "var(--mode-code)", desc: "modes.code.desc", tag: null },
  { name: "PLAN", color: "var(--mode-plan)", desc: "modes.plan.desc", tag: "read-only" },
  { name: "BUILD", color: "var(--mode-build)", desc: "modes.build.desc", tag: null },
] as const;

export function Modes() {
  const { t } = useI18n();

  return (
    <section id="modes" className="border-t border-border-subtle">
      <div className="mx-auto max-w-[1040px] px-5 py-20 sm:py-24">
        <h2 className="font-display text-[30px] font-semibold leading-[1.15] tracking-[-0.01em] text-text-primary">
          {t("modes.title")}
        </h2>
        <p className="mt-4 max-w-[560px] text-[14px] leading-[1.7] text-text-secondary">
          {t("modes.body")}
        </p>

        <div className="mt-10 divide-y divide-border-subtle border-y border-border-subtle">
          {MODES.map((m) => (
            <div
              key={m.name}
              className="grid grid-cols-[110px_1fr] items-baseline gap-4 py-4 sm:grid-cols-[140px_1fr_auto]"
            >
              <span className="flex items-center gap-2 font-mono text-[13px] font-semibold">
                <span className="soyo-mode-dot" style={{ backgroundColor: m.color }} />
                <span style={{ color: m.color }}>{m.name}</span>
              </span>
              <span className="text-[13.5px] leading-[1.6] text-text-secondary">
                {t(m.desc)}
              </span>
              {m.tag ? (
                <span className="soyo-pill col-start-2 mt-1 w-fit !text-[10.5px] sm:col-start-3 sm:mt-0">
                  {m.tag}
                </span>
              ) : (
                <span className="hidden sm:block" />
              )}
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
