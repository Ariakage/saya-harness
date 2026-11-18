import { Hand, ShieldAlert, Zap } from "lucide-react";
import { useI18n } from "@/lib/i18n";

const ITEMS = [
  {
    icon: Zap,
    iconColor: "var(--text-secondary)",
    name: "perm.auto.name",
    desc: "perm.auto.desc",
    nameClass: "text-text-primary",
  },
  {
    icon: Hand,
    iconColor: "var(--info)",
    name: "perm.manual.name",
    desc: "perm.manual.desc",
    nameClass: "text-info",
  },
  {
    icon: ShieldAlert,
    iconColor: "var(--danger)",
    name: "perm.bypass.name",
    desc: "perm.bypass.desc",
    nameClass: "text-danger",
  },
] as const;

export function Permissions() {
  const { t } = useI18n();

  return (
    <section id="permissions" className="border-t border-border-subtle">
      <div className="mx-auto max-w-[1040px] px-5 py-20 sm:py-24">
        <h2 className="font-display text-[30px] font-semibold leading-[1.15] tracking-[-0.01em] text-text-primary">
          {t("perm.title")}
        </h2>
        <p className="mt-4 max-w-[560px] text-[14px] leading-[1.7] text-text-secondary">
          {t("perm.body")}
        </p>

        <div className="mt-10 divide-y divide-border-subtle border-y border-border-subtle">
          {ITEMS.map((item) => (
            <div key={item.name} className="flex gap-4 py-5">
              <span
                className="mt-0.5 flex h-[30px] w-[30px] flex-none items-center justify-center rounded-[8px] border border-border bg-surface-raised"
                style={{ color: item.iconColor }}
              >
                <item.icon size={15} strokeWidth={1.75} />
              </span>
              <div>
                <p className={`text-[14px] font-semibold ${item.nameClass}`}>
                  {t(item.name)}
                </p>
                <p className="mt-1 max-w-[600px] text-[13.5px] leading-[1.65] text-text-secondary">
                  {t(item.desc)}
                </p>
              </div>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
