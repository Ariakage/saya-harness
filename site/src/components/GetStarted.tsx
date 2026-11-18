import { BookOpen, Download } from "lucide-react";
import { useI18n } from "@/lib/i18n";

const REPO = "https://github.com/Ariakage/soyo-harness";

export function GetStarted() {
  const { t } = useI18n();

  return (
    <section id="get-started" className="border-t border-border-subtle">
      <div className="mx-auto grid max-w-[1040px] gap-10 px-5 py-20 sm:py-24 lg:grid-cols-[minmax(0,5fr)_minmax(0,6fr)] lg:gap-14">
        <div className="flex flex-col justify-center">
          <h2 className="font-display text-[30px] font-semibold leading-[1.15] tracking-[-0.01em] text-text-primary">
            {t("start.title")}
          </h2>
          <p className="mt-4 max-w-[420px] text-[14px] leading-[1.7] text-text-secondary">
            {t("start.body")}
          </p>
          <div className="mt-7 flex flex-wrap gap-2.5">
            <a
              href={`${REPO}/releases`}
              target="_blank"
              rel="noreferrer"
              className="soyo-btn-primary"
            >
              <Download size={14} strokeWidth={1.75} />
              {t("start.releases")}
            </a>
            <a
              href={`${REPO}/blob/main/docs/usage.md`}
              target="_blank"
              rel="noreferrer"
              className="soyo-btn-secondary"
            >
              <BookOpen size={14} strokeWidth={1.75} />
              {t("start.docs")}
            </a>
          </div>
        </div>

        {/* terminal card */}
        <div className="overflow-hidden rounded-[12px] border border-border bg-surface-raised">
          <div className="flex items-center gap-1.5 border-b border-border-subtle px-4 py-3">
            <span className="h-[10px] w-[10px] rounded-full bg-[#5a5752]" />
            <span className="h-[10px] w-[10px] rounded-full bg-[#5a5752]" />
            <span className="h-[10px] w-[10px] rounded-full bg-[#5a5752]" />
            <span className="ml-2 font-mono text-[11px] text-text-tertiary">zsh</span>
          </div>
          <div className="space-y-3.5 px-4 py-5 font-mono text-[12.5px] leading-[1.7]">
            <div>
              <p className="text-text-tertiary">{t("start.step1")}</p>
              <p className="text-text-primary">
                <span className="text-accent-text">$</span> cargo build --workspace --release --locked
              </p>
            </div>
            <div>
              <p className="text-text-tertiary">{t("start.step2")}</p>
              <p className="text-text-primary">
                <span className="text-accent-text">$</span> cp examples/config.example.toml ~/.config/soyo/config.toml
              </p>
            </div>
            <div>
              <p className="text-text-tertiary">{t("start.step3")}</p>
              <p className="text-text-primary">
                <span className="text-accent-text">$</span> ./target/release/soyo --cwd /path/to/project
              </p>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
