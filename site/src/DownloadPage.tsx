import {
  Check,
  ChevronDown,
  Copy,
  Download as DownloadIcon,
  ExternalLink,
} from "lucide-react";
import { useState } from "react";
import { Footer } from "@/components/Footer";
import { Topbar } from "@/components/Topbar";
import { I18nProvider, useI18n, type MessageKey } from "@/lib/i18n";
import { initTheme } from "@/lib/theme";

initTheme();

const REPO = "https://github.com/Ariakage/soyo-harness";
const REL = `${REPO}/releases/download/v0.2.0`;

const ARCHIVES = [
  { name: "dl.plat.macArm", req: "dl.plat.reqMac", file: "saya-0.2.0-macos-aarch64.tar.gz" },
  { name: "dl.plat.macIntel", req: "dl.plat.reqMac", file: "saya-0.2.0-macos-x86_64.tar.gz" },
  { name: "dl.plat.linuxX64", req: "dl.plat.reqLinux", file: "saya-0.2.0-linux-x86_64.tar.gz" },
  { name: "dl.plat.linuxArm", req: "dl.plat.reqLinux", file: "saya-0.2.0-linux-aarch64.tar.gz" },
] as const;

const FAQS: { q: MessageKey; a: MessageKey; cmd?: MessageKey }[] = [
  { q: "dl.faq.q1", a: "dl.faq.a1" },
  { q: "dl.faq.q2", a: "dl.faq.a2" },
  { q: "dl.faq.q3", a: "dl.faq.a3", cmd: "dl.faq.a3.cmd" },
  { q: "dl.faq.q4", a: "dl.faq.a4", cmd: "dl.faq.a4.cmd" },
  { q: "dl.faq.q5", a: "dl.faq.a5" },
  { q: "dl.faq.q6", a: "dl.faq.a6" },
];

function detectOs(): "mac" | "linux" | "other" {
  if (typeof navigator === "undefined") return "other";
  const ua = navigator.userAgent;
  if (/Mac OS X|macOS|Macintosh/i.test(ua)) return "mac";
  if (/Linux/i.test(ua) && !/Android/i.test(ua)) return "linux";
  return "other";
}

function CopyButton({ text, label }: { text: string; label: string }) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      aria-label={label}
      title={copied ? t("hero.copied") : label}
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(text);
          setCopied(true);
          setTimeout(() => setCopied(false), 1400);
        } catch {
          /* clipboard unavailable */
        }
      }}
      className="inline-flex h-[24px] w-[24px] items-center justify-center rounded-[6px] text-text-tertiary transition-colors hover:bg-surface-hover hover:text-text-primary"
    >
      {copied ? (
        <Check size={13} strokeWidth={2} className="text-success" />
      ) : (
        <Copy size={13} strokeWidth={1.75} />
      )}
    </button>
  );
}

function DownloadHero() {
  const { t } = useI18n();
  const os = detectOs();
  const cta =
    os === "mac"
      ? { href: `${REL}/saya-0.2.0-macos-aarch64.tar.gz`, label: t("dl.hero.cta.mac") }
      : os === "linux"
        ? { href: `${REL}/saya-0.2.0-linux-x86_64.tar.gz`, label: t("dl.hero.cta.linux") }
        : { href: "#platforms", label: t("dl.hero.cta") };

  return (
    <section className="mx-auto max-w-[720px] px-5 pb-14 pt-20 text-center sm:pt-24">
      <div className="mx-auto flex w-fit items-center gap-2 rounded-[8px] border border-border bg-surface-raised px-3 py-1.5">
        <img src="/app-icon.png" alt="" className="h-[18px] w-[18px] rounded-[22%]" />
        <span className="text-[12px] font-medium text-text-secondary">
          {t("dl.hero.eyebrow")}
        </span>
      </div>
      <h1 className="mt-6 font-display text-[42px] font-semibold leading-[1.08] tracking-[-0.015em] text-text-primary sm:text-[54px]">
        {t("dl.hero.title")}
      </h1>
      <p className="mx-auto mt-4 max-w-[480px] text-[14.5px] leading-[1.7] text-text-secondary">
        {t("dl.hero.sub")}
      </p>
      <div className="mt-8 flex flex-wrap items-center justify-center gap-2.5">
        <a href={cta.href} className="soyo-btn-primary">
          <DownloadIcon size={14} strokeWidth={1.75} />
          {cta.label}
        </a>
        <a
          href={`${REPO}/releases`}
          target="_blank"
          rel="noreferrer"
          className="soyo-btn-secondary"
        >
          {t("dl.hero.releases")}
        </a>
      </div>
      <p className="mt-5 text-[12px] text-text-tertiary">{t("dl.hero.meta")}</p>
    </section>
  );
}

function Platforms() {
  const { t } = useI18n();
  return (
    <section id="platforms" className="border-t border-border-subtle">
      <div className="mx-auto max-w-[880px] px-5 py-16 sm:py-20">
        <h2 className="font-display text-[28px] font-semibold leading-[1.15] tracking-[-0.01em] text-text-primary">
          {t("dl.plat.title")}
        </h2>
        <p className="mt-3 max-w-[560px] text-[13.5px] leading-[1.7] text-text-secondary">
          {t("dl.plat.sub")}
        </p>

        <div className="mt-8 divide-y divide-border-subtle overflow-hidden rounded-[12px] border border-border bg-surface-raised">
          {ARCHIVES.map((a) => (
            <div
              key={a.file}
              className="flex flex-col gap-2 px-4 py-3.5 sm:flex-row sm:items-center sm:gap-4"
            >
              <div className="min-w-0 flex-1">
                <p className="text-[13.5px] font-medium text-text-primary">
                  {t(a.name)}
                </p>
                <p className="mt-0.5 truncate font-mono text-[11px] text-text-tertiary">
                  {a.file}
                </p>
              </div>
              <span className="w-fit shrink-0 font-mono text-[11.5px] text-text-tertiary">
                {t(a.req)}
              </span>
              <a
                href={`${REL}/${a.file}`}
                className="inline-flex h-[30px] w-fit shrink-0 items-center gap-1.5 rounded-[8px] border border-border-strong bg-surface-overlay px-3 text-[12.5px] font-medium text-text-primary transition-colors hover:bg-surface-hover"
              >
                <DownloadIcon size={13} strokeWidth={1.75} />
                {t("dl.plat.download")}
              </a>
            </div>
          ))}
          <div className="flex flex-col gap-2 px-4 py-3.5 opacity-60 sm:flex-row sm:items-center sm:gap-4">
            <div className="min-w-0 flex-1">
              <p className="text-[13.5px] font-medium text-text-primary">
                {t("dl.plat.windows")}
              </p>
            </div>
            <span className="w-fit shrink-0 font-mono text-[11.5px] text-text-tertiary">
              —
            </span>
            <span className="inline-flex h-[30px] w-fit shrink-0 items-center rounded-[8px] border border-border px-3 text-[12.5px] text-text-tertiary">
              {t("dl.plat.winSoon")}
            </span>
          </div>
        </div>

        <p className="mt-4 text-[12px] leading-[1.7] text-text-tertiary">
          {t("dl.plat.rename")}
        </p>

        {/* verification strip */}
        <div className="mt-8 rounded-[12px] border border-border bg-surface-raised p-4 sm:p-5">
          <p className="text-[13px] font-semibold text-text-primary">
            {t("dl.verify.title")}
          </p>
          <p className="mt-1.5 max-w-[560px] text-[12.5px] leading-[1.7] text-text-secondary">
            {t("dl.verify.body")}
          </p>
          <div className="mt-3.5 flex items-center gap-2 rounded-[8px] border border-border-subtle bg-surface-app px-3 py-2 font-mono text-[12px]">
            <span className="text-accent-text">$</span>
            <code className="min-w-0 flex-1 truncate text-text-primary">
              shasum -a 256 -c SHA256SUMS
            </code>
            <CopyButton text="shasum -a 256 -c SHA256SUMS" label={t("hero.copy")} />
          </div>
          <div className="mt-3 flex flex-wrap gap-x-4 gap-y-1.5 text-[12px]">
            {[
              { label: t("dl.verify.sums"), href: `${REL}/SHA256SUMS` },
              { label: t("dl.verify.evidence"), href: `${REL}/release-evidence.json` },
              {
                label: t("dl.verify.how"),
                href: `${REPO}/blob/main/docs/releases/release-validation.md`,
              },
            ].map((l) => (
              <a
                key={l.href}
                href={l.href}
                target="_blank"
                rel="noreferrer"
                className="inline-flex items-center gap-1 text-accent-text transition-colors hover:underline"
              >
                {l.label}
                <ExternalLink size={11} strokeWidth={1.75} />
              </a>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}

const SURFACES = [
  { name: "surfaces.cli.name", desc: "surfaces.cli.desc", shipped: true },
  { name: "surfaces.tui.name", desc: "surfaces.tui.desc", shipped: true },
  { name: "surfaces.gui.name", desc: "surfaces.gui.desc", shipped: false },
] as const;

function SurfacesRow() {
  const { t } = useI18n();
  return (
    <section className="border-t border-border-subtle">
      <div className="mx-auto max-w-[880px] px-5 py-16 sm:py-20">
        <h2 className="font-display text-[28px] font-semibold leading-[1.15] tracking-[-0.01em] text-text-primary">
          {t("dl.surf.title")}
        </h2>
        <p className="mt-3 max-w-[560px] text-[13.5px] leading-[1.7] text-text-secondary">
          {t("dl.surf.sub")}
        </p>
        <div className="mt-8 divide-y divide-border-subtle border-y border-border-subtle">
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
              <span className="soyo-pill col-start-2 mt-1 w-fit !text-[10.5px] sm:col-start-3 sm:mt-0">
                <span
                  className={`soyo-mode-dot ${s.shipped ? "bg-success" : "bg-warning"}`}
                />
                {t(s.shipped ? "surfaces.shipped" : "surfaces.dev")}
              </span>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}

const SOURCE_LINES = [
  "git clone https://github.com/Ariakage/soyo-harness.git",
  "cd soyo-harness",
  "cargo install --path crates/soyo-cli --locked",
  "# or: cargo build --workspace --release --locked",
] as const;

function SourceBuild() {
  const { t } = useI18n();
  return (
    <section className="border-t border-border-subtle">
      <div className="mx-auto max-w-[880px] px-5 py-16 sm:py-20">
        <h2 className="font-display text-[28px] font-semibold leading-[1.15] tracking-[-0.01em] text-text-primary">
          {t("dl.src.title")}
        </h2>
        <p className="mt-3 max-w-[560px] text-[13.5px] leading-[1.7] text-text-secondary">
          {t("dl.src.body")}
        </p>
        <div className="relative mt-8 overflow-hidden rounded-[12px] border border-border bg-surface-raised">
          <div className="flex items-center justify-between border-b border-border-subtle px-4 py-2.5">
            <span className="font-mono text-[11px] text-text-tertiary">zsh</span>
            <CopyButton text={SOURCE_LINES.slice(0, 3).join("\n")} label={t("hero.copy")} />
          </div>
          <div className="space-y-1 px-4 py-4 font-mono text-[12.5px] leading-[1.8]">
            <p className="text-text-tertiary">{t("dl.src.clone")}</p>
            <p className="text-text-primary">
              <span className="text-accent-text">$</span> {SOURCE_LINES[0]}
            </p>
            <p className="text-text-tertiary">{t("dl.src.install")}</p>
            <p className="text-text-primary">
              <span className="text-accent-text">$</span> {SOURCE_LINES[1]} && {SOURCE_LINES[2]}
            </p>
            <p className="text-text-tertiary">{t("dl.src.build")}</p>
            <p className="text-text-primary">
              <span className="text-accent-text">$</span> cargo build --workspace --release --locked
            </p>
          </div>
        </div>
      </div>
    </section>
  );
}

function Faq() {
  const { t } = useI18n();
  return (
    <section className="border-t border-border-subtle">
      <div className="mx-auto max-w-[880px] px-5 py-16 sm:py-20">
        <h2 className="font-display text-[28px] font-semibold leading-[1.15] tracking-[-0.01em] text-text-primary">
          {t("dl.faq.title")}
        </h2>
        <div className="mt-8 divide-y divide-border-subtle border-y border-border-subtle">
          {FAQS.map((f) => (
            <details key={f.q} name="dl-faq" className="group py-1">
              <summary className="flex cursor-pointer list-none items-center gap-3 py-3.5 text-[13.5px] font-medium text-text-primary transition-colors hover:text-accent-text [&::-webkit-details-marker]:hidden">
                <span className="flex-1">{t(f.q)}</span>
                <ChevronDown
                  size={14}
                  strokeWidth={1.75}
                  className="shrink-0 text-text-tertiary transition-transform duration-150 group-open:rotate-180"
                />
              </summary>
              <div className="pb-4 pr-8 text-[13px] leading-[1.75] text-text-secondary">
                {t(f.a)}
                {f.cmd && (
                  <code className="mt-2.5 block w-fit whitespace-pre-wrap rounded-[8px] border border-border-subtle bg-surface-raised px-3 py-2 font-mono text-[11.5px] text-text-primary">
                    {t(f.cmd)}
                  </code>
                )}
              </div>
            </details>
          ))}
        </div>

        <div className="mt-12 text-center">
          <a
            href="/#loop"
            className="text-[13px] font-medium text-accent-text transition-colors hover:underline"
          >
            ← {t("dl.back")}
          </a>
        </div>
      </div>
    </section>
  );
}

export default function DownloadPage() {
  return (
    <I18nProvider>
      <div className="min-h-screen bg-surface-app text-text-primary">
        <Topbar base="/" />
        <main>
          <DownloadHero />
          <Platforms />
          <SurfacesRow />
          <SourceBuild />
          <Faq />
        </main>
        <Footer />
      </div>
    </I18nProvider>
  );
}
