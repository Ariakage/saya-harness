import { BookOpen, Check, Copy, Download } from "lucide-react";
import { useMemo, useState } from "react";
import { AppWindow } from "@/components/AppWindow";
import { pickGreeting, useI18n } from "@/lib/i18n";

const REPO = "https://github.com/Ariakage/soyo-harness";

export function Hero() {
  const { lang, t } = useI18n();
  const greeting = useMemo(() => pickGreeting(lang), [lang]);
  const [copied, setCopied] = useState(false);

  const command = t("hero.command");

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(command);
    } catch {
      const ta = document.createElement("textarea");
      ta.value = command;
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      ta.remove();
    }
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1600);
  };

  return (
    <section id="top" className="relative">
      <div className="mx-auto flex max-w-[1040px] flex-col items-center px-5 pb-16 pt-16 sm:pt-20">
        {/* eyebrow: icon + product name, greeting */}
        <div className="soyo-enter flex items-center gap-2.5">
          <img
            src="/app-icon.png"
            alt="Soyo Harness"
            className="h-[40px] w-[40px] rounded-[22%]"
          />
          <div className="text-left">
            <p className="text-[14px] font-semibold leading-tight text-text-primary">
              Soyo Harness
            </p>
            <p className="text-[11.5px] leading-tight text-text-tertiary">
              {greeting}
            </p>
          </div>
        </div>

        <h1 className="soyo-enter soyo-enter-1 font-display mt-8 max-w-[780px] text-center text-[40px] font-semibold leading-[1.12] tracking-[-0.015em] text-text-primary sm:text-[54px]">
          {t("hero.title")}
        </h1>

        <p className="soyo-enter soyo-enter-2 mt-5 max-w-[600px] text-center text-[15px] leading-[1.65] text-text-secondary">
          {t("hero.tagline")}
        </p>

        <div className="soyo-enter soyo-enter-2 mt-8 flex flex-wrap items-center justify-center gap-2.5">
          <a href="#get-started" className="soyo-btn-primary">
            <Download size={14} strokeWidth={1.75} />
            {t("hero.cta.start")}
          </a>
          <a
            href={`${REPO}/blob/main/docs/usage.md`}
            target="_blank"
            rel="noreferrer"
            className="soyo-btn-secondary"
          >
            <BookOpen size={14} strokeWidth={1.75} />
            {t("hero.cta.docs")}
          </a>
        </div>

        {/* slim install command chip */}
        <button
          type="button"
          onClick={copy}
          title={t("hero.copy")}
          aria-label={t("hero.copy")}
          className="soyo-enter soyo-enter-3 group mt-6 flex max-w-full items-center gap-2.5 rounded-[8px] border border-border bg-surface-raised px-3.5 py-2 transition-colors hover:border-border-strong"
        >
          <code className="truncate text-[12.5px] text-text-primary">
            {command}
          </code>
          {copied ? (
            <Check size={14} strokeWidth={1.75} className="flex-none text-success" />
          ) : (
            <Copy
              size={14}
              strokeWidth={1.75}
              className="flex-none text-text-tertiary transition-colors group-hover:text-text-primary"
            />
          )}
          <span className="sr-only">{copied ? t("hero.copied") : t("hero.copy")}</span>
        </button>
        <p className="mt-1.5 h-[14px] text-[11px] text-accent-text">
          {copied ? t("hero.copied") : ""}
        </p>

        <p className="soyo-enter soyo-enter-3 mt-3 text-[12px] text-text-tertiary">
          {t("hero.availability")}
        </p>
        <p className="soyo-enter soyo-enter-3 mt-1.5 text-[12px] text-text-tertiary">
          {t("hero.surfaces")}
        </p>

        {/* product window */}
        <div className="mt-14 w-full max-w-[960px]">
          <AppWindow />
        </div>
      </div>
    </section>
  );
}
