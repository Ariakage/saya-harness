import { useI18n } from "@/lib/i18n";

const SURFACES = [
  { name: "surfaces.cli.name", desc: "surfaces.cli.desc", shipped: true },
  { name: "surfaces.tui.name", desc: "surfaces.tui.desc", shipped: true },
  { name: "surfaces.gui.name", desc: "surfaces.gui.desc", shipped: false },
] as const;

const CAPABILITIES = [
  "OpenAI Chat Completions",
  "OpenAI Responses",
  "Anthropic Messages",
  "MCP stdio / HTTP",
  "SQLite WAL sessions",
  "Plugins & hooks",
  "Global memory",
  "Goal orchestration",
  "Managed browser",
  "Context compaction",
];

export function Surfaces() {
  const { t } = useI18n();

  return (
    <section id="surfaces" className="border-t border-border-subtle">
      <div className="mx-auto max-w-[1040px] px-5 py-20 sm:py-24">
        <h2 className="font-display text-[30px] font-semibold leading-[1.15] tracking-[-0.01em] text-text-primary">
          {t("surfaces.title")}
        </h2>
        <p className="mt-4 max-w-[560px] text-[14px] leading-[1.7] text-text-secondary">
          {t("surfaces.body")}
        </p>

        <div className="mt-10 divide-y divide-border-subtle border-y border-border-subtle">
          {SURFACES.map((s) => (
            <div
              key={s.name}
              className="grid grid-cols-[72px_1fr] items-baseline gap-4 py-4 sm:grid-cols-[90px_1fr_auto]"
            >
              <span className="font-mono text-[13px] font-semibold text-text-primary">
                {t(s.name)}
              </span>
              <span className="text-[13.5px] leading-[1.6] text-text-secondary">
                {t(s.desc)}
              </span>
              {s.shipped ? (
                <span className="soyo-pill col-start-2 mt-1 w-fit !text-[10.5px] sm:col-start-3 sm:mt-0">
                  <span className="soyo-mode-dot bg-success" />
                  {t("surfaces.shipped")}
                </span>
              ) : (
                <span className="soyo-pill col-start-2 mt-1 w-fit !text-[10.5px] sm:col-start-3 sm:mt-0">
                  <span className="soyo-mode-dot bg-warning" />
                  {t("surfaces.dev")}
                </span>
              )}
            </div>
          ))}
        </div>

        <p className="mt-12 text-[12px] font-medium text-text-tertiary">
          {t("surfaces.strip.title")}
        </p>
        <div className="mt-3 flex flex-wrap gap-2">
          {CAPABILITIES.map((c) => (
            <span key={c} className="soyo-pill font-mono !text-[11px]">
              {c}
            </span>
          ))}
        </div>
      </div>
    </section>
  );
}
