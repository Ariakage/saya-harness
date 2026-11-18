import { ExternalLink, Languages, Monitor, Moon, Sun } from "lucide-react";
import { useI18n } from "@/lib/i18n";
import { cycleThemePref, useThemePref } from "@/lib/theme";

const REPO = "https://github.com/Ariakage/soyo-harness";

export function Topbar({ base = "" }: { base?: string }) {
  const { lang, setLang, t } = useI18n();
  const pref = useThemePref();

  const ThemeIcon = pref === "light" ? Sun : pref === "dark" ? Moon : Monitor;
  const themeLabel =
    pref === "system" ? "Theme: system" : pref === "light" ? "Theme: light" : "Theme: dark";

  const links: { href: string; label: string }[] = [
    { href: `${base}#loop`, label: t("nav.loop") },
    { href: `${base}#modes`, label: t("nav.modes") },
    { href: `${base}#permissions`, label: t("nav.permissions") },
    { href: `${base}#isolation`, label: t("nav.isolation") },
    { href: `${base}#surfaces`, label: t("nav.surfaces") },
    { href: "download.html", label: t("nav.download") },
  ];

  return (
    <header className="sticky top-0 z-50 border-b border-border-subtle bg-surface-sidebar/85 backdrop-blur-md">
      <div className="mx-auto flex h-[52px] max-w-[1040px] items-center gap-3 px-5">
        <a href={base || "#top"} className="flex items-center gap-2.5">
          <img
            src="/app-icon.png"
            alt=""
            className="h-[22px] w-[22px] rounded-[22%]"
          />
          <span className="text-[13.5px] font-semibold tracking-[-0.01em] text-text-primary">
            Soyo Harness
          </span>
        </a>

        <nav className="ml-6 hidden items-center gap-1 md:flex" aria-label="Sections">
          {links.map((l) => (
            <a
              key={l.href}
              href={l.href}
              className="rounded-[6px] px-2.5 py-1.5 text-[12.5px] text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary"
            >
              {l.label}
            </a>
          ))}
        </nav>

        <div className="ml-auto flex items-center gap-1.5">
          <button
            type="button"
            className="soyo-pill-btn !px-2.5"
            onClick={() => setLang(lang === "en" ? "zh-CN" : "en")}
            aria-label={t("lang.label")}
            title={t("lang.label")}
          >
            <Languages size={14} strokeWidth={1.75} />
            <span className="text-[11.5px]">{lang === "en" ? "EN" : "中"}</span>
          </button>

          <button
            type="button"
            className="soyo-pill-btn !px-2.5"
            onClick={() => cycleThemePref()}
            aria-label={themeLabel}
            title={themeLabel}
          >
            <ThemeIcon size={14} strokeWidth={1.75} />
          </button>

          <a
            href={REPO}
            target="_blank"
            rel="noreferrer"
            className="soyo-pill-btn"
          >
            <ExternalLink size={14} strokeWidth={1.75} />
            <span className="hidden sm:inline">{t("nav.github")}</span>
          </a>
        </div>
      </div>
    </header>
  );
}
