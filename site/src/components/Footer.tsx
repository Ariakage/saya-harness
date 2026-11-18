import { useI18n } from "@/lib/i18n";

const REPO = "https://github.com/Ariakage/soyo-harness";

export function Footer() {
  const { t } = useI18n();

  const links = [
    { label: t("footer.github"), href: REPO },
    { label: t("footer.releases"), href: `${REPO}/releases` },
    { label: t("footer.issues"), href: `${REPO}/issues` },
    { label: t("footer.docs"), href: `${REPO}/tree/main/docs` },
    { label: t("footer.support"), href: `${REPO}/blob/main/SUPPORT.md` },
  ];

  return (
    <footer className="border-t border-border-subtle bg-surface-sidebar">
      <div className="mx-auto flex max-w-[1040px] flex-col gap-6 px-5 py-10 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex items-center gap-3">
          <img
            src="/app-icon.png"
            alt=""
            className="h-[30px] w-[30px] rounded-[22%]"
          />
          <div>
            <p className="text-[13px] font-semibold text-text-primary">
              Soyo Harness
            </p>
            <p className="text-[11.5px] text-text-tertiary">
              {t("footer.tagline")} · {t("footer.license")} · {t("footer.formerly")}
            </p>
          </div>
        </div>

        <nav className="flex flex-wrap items-center gap-x-5 gap-y-2" aria-label="Footer">
          {links.map((l) => (
            <a
              key={l.href}
              href={l.href}
              target="_blank"
              rel="noreferrer"
              className="text-[12.5px] text-text-secondary transition-colors hover:text-text-primary"
            >
              {l.label}
            </a>
          ))}
        </nav>
      </div>
    </footer>
  );
}
