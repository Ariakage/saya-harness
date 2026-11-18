import { Globe, KeyRound, ShieldCheck, SquareTerminal } from "lucide-react";
import { useI18n } from "@/lib/i18n";

const ITEMS = [
  { icon: ShieldCheck, title: "isolation.item1.title", desc: "isolation.item1.desc" },
  { icon: Globe, title: "isolation.item2.title", desc: "isolation.item2.desc" },
  { icon: SquareTerminal, title: "isolation.item3.title", desc: "isolation.item3.desc" },
  { icon: KeyRound, title: "isolation.item4.title", desc: "isolation.item4.desc" },
] as const;

export function Isolation() {
  const { t } = useI18n();

  return (
    <section id="isolation" className="border-t border-border-subtle">
      <div className="mx-auto max-w-[1040px] px-5 py-20 sm:py-24">
        <h2 className="font-display text-[30px] font-semibold leading-[1.15] tracking-[-0.01em] text-text-primary">
          {t("isolation.title")}
        </h2>
        <p className="mt-4 max-w-[560px] text-[14px] leading-[1.7] text-text-secondary">
          {t("isolation.body")}
        </p>

        <div className="mt-10 grid gap-px overflow-hidden rounded-[12px] border border-border-subtle bg-border-subtle sm:grid-cols-2">
          {ITEMS.map((item) => (
            <div key={item.title} className="bg-surface-raised p-5">
              <span className="flex h-[30px] w-[30px] items-center justify-center rounded-[8px] border border-border bg-surface-app text-accent-text">
                <item.icon size={15} strokeWidth={1.75} />
              </span>
              <p className="mt-3 text-[14px] font-semibold text-text-primary">
                {t(item.title)}
              </p>
              <p className="mt-1.5 text-[13px] leading-[1.65] text-text-secondary">
                {t(item.desc)}
              </p>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
