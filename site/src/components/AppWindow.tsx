import {
  ChevronDown,
  ChevronRight,
  ChevronUp,
  Code2,
  Database,
  Download,
  Flag,
  Folder,
  Gauge,
  Globe,
  HelpCircle,
  Home,
  List,
  MessageSquare,
  MoreHorizontal,
  PanelLeft,
  PanelRight,
  Paperclip,
  Pause,
  Pencil,
  Pin,
  Plus,
  Search,
  Settings,
  Shield,
  Square,
  Terminal,
} from "lucide-react";
import { useI18n } from "@/lib/i18n";

/* A faithful replica of the real Soyo Desktop window (desktop/src/App.tsx):
   38px unified titlebar, 48px primary rail, 248px sessions sidebar,
   borderless message stream with tool rows + inline diff, docked composer.
   The app ships Soyo Dark only, so the window stays dark in both site themes. */

function ToolStatus({ kind }: { kind: "done" | "error" }) {
  const { t } = useI18n();
  return (
    <span
      className={`inline-flex items-center gap-[5px] rounded-[8px] border border-[#2e2e2e] bg-[#1c1c1c] px-[10px] py-[3px] text-[10.5px] font-medium ${
        kind === "done" ? "text-[#4ade80]" : "text-[#f87171]"
      }`}
    >
      {t(kind === "done" ? "win.done" : "win.error")}
    </span>
  );
}

function RailIcon({
  icon: Icon,
  active,
}: {
  icon: typeof Home;
  active?: boolean;
}) {
  return (
    <span className="relative flex h-9 w-9 items-center justify-center rounded-[9px]">
      {active && (
        <span className="absolute -left-[5px] h-4 w-[2px] rounded-r-full bg-[#d9a08e]" />
      )}
      <Icon
        size={16}
        strokeWidth={1.75}
        className={active ? "text-[#e8e8e8]" : "text-[#8a8a8a]"}
      />
    </span>
  );
}

const DIFF_LINES: {
  kind: "hunk" | "add" | "del" | "ctx";
  ln: string;
  code: string;
}[] = [
  { kind: "hunk", ln: "", code: "@@ -31,3 +31,3 @@ fn assert_turns" },
  { kind: "ctx", ln: "31", code: "  let turns = run_loop(&cfg).await?;" },
  { kind: "del", ln: "32", code: "- assert_eq!(turns, 3);" },
  { kind: "add", ln: "32", code: "+ assert_eq!(turns, 4);" },
];

export function AppWindow() {
  const { t } = useI18n();

  return (
    <div className="overflow-hidden rounded-[14px] border border-[#262626] bg-[#101010] text-left text-[#e8e8e8] shadow-dialog">
      {/* 38px unified titlebar (#0f0f0f) */}
      <div className="flex h-[38px] items-center border-b border-[#1f1f1f] bg-[#0f0f0f]">
        {/* left zone — traffic lights, sidebar toggle, view switch */}
        <div className="flex h-full shrink-0 items-center gap-1.5 pl-3 pr-2 md:w-[296px] md:border-r md:border-[#1f1f1f] md:pl-[76px]">
          <span className="flex gap-1.5 pr-1 md:hidden">
            <span className="h-[10px] w-[10px] rounded-full bg-[#3d3d3d]" />
            <span className="h-[10px] w-[10px] rounded-full bg-[#3d3d3d]" />
            <span className="h-[10px] w-[10px] rounded-full bg-[#3d3d3d]" />
          </span>
          <span className="hidden gap-1.5 md:absolute md:left-4 md:flex">
            <span className="h-[10px] w-[10px] rounded-full bg-[#3d3d3d]" />
            <span className="h-[10px] w-[10px] rounded-full bg-[#3d3d3d]" />
            <span className="h-[10px] w-[10px] rounded-full bg-[#3d3d3d]" />
          </span>
          <span className="flex h-[26px] w-[26px] items-center justify-center rounded-[6px] text-[#8a8a8a]">
            <PanelLeft size={15} strokeWidth={1.75} />
          </span>
          <span className="ml-auto inline-flex h-[26px] items-center rounded-[7px] border border-[#242424] bg-[#161616] p-0.5 md:ml-0">
            <span className="flex h-[20px] w-[26px] items-center justify-center rounded-[5px] bg-[#262626] text-[#e8e8e8]">
              <MessageSquare size={13} strokeWidth={1.75} />
            </span>
            <span className="flex h-[20px] w-[26px] items-center justify-center rounded-[5px] text-[#6e6e6e]">
              <Code2 size={13} strokeWidth={1.75} />
            </span>
          </span>
        </div>

        {/* right zone — session switcher, goal pill, mode/perm, actions */}
        <div className="flex h-full min-w-0 flex-1 items-center gap-2 px-3">
          <span className="flex h-[26px] min-w-0 items-center gap-1.5 rounded-[6px] px-2 text-[12.5px]">
            <span className="truncate font-medium text-[#d9d9d9]">
              {t("win.title")}
            </span>
            <span className="hidden shrink-0 text-[#6b6b6b] sm:inline">
              · {t("win.project")}
            </span>
            <ChevronDown size={13} strokeWidth={1.75} className="shrink-0 text-[#6e6e6e]" />
          </span>

          <span className="min-w-[16px] flex-1" />

          <span className="hidden items-center gap-1 xl:flex">
            {/* Goal pill — active run (amber) */}
            <span className="inline-flex items-center gap-2 rounded-[8px] border border-[#3d3527] bg-[#1c1a16] px-2.5 py-1 text-[11.5px] text-[#e8d9b0]">
              <Flag size={11} strokeWidth={1.75} />
              <span className="max-w-[140px] truncate">{t("win.title")}</span>
              <span className="flex h-4 w-4 items-center justify-center rounded-[5px] border border-[#3d3527] bg-[#262019] text-[#c9b890]">
                <Pause size={8} strokeWidth={2} />
              </span>
              <span className="flex h-4 w-4 items-center justify-center rounded-[5px] border border-[#3d3527] bg-[#262019] text-[#c9b890]">
                <Square size={7} strokeWidth={2} className="fill-current" />
              </span>
            </span>

            {/* mode pill */}
            <span className="inline-flex h-[26px] items-center gap-1.5 rounded-[6px] border border-[#262626] bg-[#181818] px-2.5 text-[11.5px]">
              <span className="h-1.5 w-1.5 rounded-full bg-[#4ade80]" />
              <span className="text-[#4ade80]">CODE</span>
              <ChevronDown size={11} strokeWidth={1.75} className="text-[#6e6e6e]" />
            </span>

            {/* permission pill */}
            <span className="inline-flex h-[26px] items-center gap-1.5 rounded-[6px] border border-[#262626] bg-[#181818] px-2.5 text-[11.5px]">
              <span className="text-[#8aa8d8]">auto</span>
              <ChevronDown size={11} strokeWidth={1.75} className="text-[#6e6e6e]" />
            </span>
          </span>

          <span className="hidden items-center gap-1 sm:flex">
            <PanelRight size={13} strokeWidth={1.75} className="text-[#8a8a8a]" />
            <Globe size={13} strokeWidth={1.75} className="text-[#8a8a8a]" />
            <Folder size={13} strokeWidth={1.75} className="text-[#8a8a8a]" />
            <MoreHorizontal size={15} strokeWidth={1.75} className="text-[#a3a3a3]" />
          </span>
        </div>
      </div>

      <div className="flex">
        {/* Column 1: 48px primary rail (#0d0d0d) */}
        <div className="hidden w-[48px] flex-none flex-col items-center gap-0.5 border-r border-[#1f1f1f] bg-[#0d0d0d] py-2.5 sm:flex">
          <RailIcon icon={Home} />
          <RailIcon icon={MessageSquare} active />
          <RailIcon icon={Search} />
          <RailIcon icon={Database} />
          <RailIcon icon={Shield} />
          <RailIcon icon={Download} />
          <RailIcon icon={Settings} />
          <div className="mt-auto flex flex-col items-center gap-1.5">
            <RailIcon icon={HelpCircle} />
            <img
              src="/app-icon.png"
              alt=""
              className="h-6 w-6 rounded-[22%] object-cover ring-1 ring-[#2a2a2a]"
            />
          </div>
        </div>

        {/* Column 2: 248px sessions sidebar (#0f0f0f) */}
        <div className="hidden w-[248px] flex-none flex-col border-r border-[#1f1f1f] bg-[#0f0f0f] p-2.5 text-xs md:flex">
          <div className="flex items-center justify-between px-1 pb-2 pt-0.5">
            <span className="text-[14px] font-semibold text-[#e8e8e8]">
              {t("win.sessions")}
            </span>
            <MoreHorizontal size={15} strokeWidth={1.75} className="text-[#8a8a8a]" />
          </div>
          <p className="flex items-center gap-2 rounded-[7px] px-2 py-1.5 text-[13px] font-medium text-[#d9a08e]">
            <Plus size={13} strokeWidth={1.75} />
            {t("win.new")}
          </p>

          <div className="mt-2.5 space-y-2.5">
            <div>
              <div className="px-2 py-1 text-[11px] text-[#6b6b6b]">
                {t("win.pinned")}
              </div>
              <div className="rounded-[8px] px-2.5 py-1.5">
                <div className="flex items-center gap-1.5 text-[12.5px] text-[#d9d9d9]">
                  <Pin size={11} strokeWidth={1.75} className="shrink-0 text-[#8a8a8a]" />
                  <span className="truncate">{t("win.pinnedTitle")}</span>
                </div>
                <div className="mt-0.5 pl-[18px] text-[11px] text-[#d9a08e]">
                  {t("win.pinnedSub")}
                </div>
              </div>
            </div>

            <div className="space-y-0.5">
              <div className="flex items-center justify-between px-2 py-1 text-[11px] text-[#6b6b6b]">
                <span className="inline-flex items-center gap-1.5 truncate">
                  <Folder size={11} strokeWidth={1.75} className="shrink-0" />
                  <span className="truncate">{t("win.project")}</span>
                </span>
                <span>4</span>
              </div>
              {(
                [
                  ["win.recent1", "win.recent1.sub", true],
                  ["win.recent2", "win.recent2.sub", false],
                  ["win.recent3", "win.recent3.sub", false],
                  ["win.recent4", "win.recent4.sub", false],
                ] as const
              ).map(([title, sub, active]) => (
                <div
                  key={title}
                  className={`flex flex-col gap-0.5 rounded-[8px] px-2.5 py-1.5 ${
                    active ? "bg-[#1c1c1c] text-[#e8e8e8]" : "text-[#a3a3a3]"
                  }`}
                >
                  <span className="truncate text-[12.5px] font-medium">
                    {t(title)}
                  </span>
                  <span className="truncate text-[11px] text-[#6e6e6e]">
                    {t(sub)}
                  </span>
                </div>
              ))}
            </div>
          </div>
          <p className="mt-auto px-2.5 py-1.5 text-[12px] text-[#6e6e6e]">
            {t("win.viewAll")}
          </p>
        </div>

        {/* Column 3: main surface (#141414) */}
        <div className="flex min-w-0 flex-1 flex-col bg-[#141414]">
          {/* message stream — assistant text is borderless (§4.2) */}
          <div className="flex-1 space-y-3.5 px-4 pb-3 pt-4 sm:px-6">
            <div className="flex justify-end">
              <div className="max-w-[85%] rounded-[14px] border border-[#2e2e2e] bg-[#242424] px-3.5 py-2 text-[13.5px] leading-relaxed text-[#ececec]">
                {t("loop.demo.user")}
              </div>
            </div>

            <p className="py-0.5 text-[13.5px] leading-relaxed text-[#e8e8e8]">
              {t("loop.demo.thinking")}
            </p>

            {/* tool rows — borderless, icon + mono name + status + summary + chevron */}
            <div className="flex items-center gap-2.5 rounded-[8px] px-1 py-[6px] text-[12.5px]">
              <Terminal size={13} strokeWidth={1.75} className="shrink-0 text-[#8a8a8a]" />
              <span className="truncate font-mono text-[#e8e8e8]">exec</span>
              <ToolStatus kind="error" />
              <span className="flex-1" />
              <span className="max-w-[220px] truncate font-mono text-[11.5px] text-[#6e6e6e]">
                {t("win.exec.summary")}
              </span>
              <ChevronRight size={13} strokeWidth={1.75} className="shrink-0 text-[#6e6e6e]" />
            </div>

            <div className="flex items-center gap-2.5 rounded-[8px] px-1 py-[6px] text-[12.5px]">
              <Terminal size={13} strokeWidth={1.75} className="shrink-0 text-[#8a8a8a]" />
              <span className="truncate font-mono text-[#e8e8e8]">read_file</span>
              <ToolStatus kind="done" />
              <span className="flex-1" />
              <span className="max-w-[220px] truncate font-mono text-[11.5px] text-[#6e6e6e]">
                {t("win.read.summary")}
              </span>
              <ChevronRight size={13} strokeWidth={1.75} className="shrink-0 text-[#6e6e6e]" />
            </div>

            {/* edit_file — expanded with inline diff */}
            <div className="text-[12.5px]">
              <div className="flex items-center gap-2.5 rounded-[8px] px-1 py-[6px]">
                <Pencil size={13} strokeWidth={1.75} className="shrink-0 text-[#8a8a8a]" />
                <span className="truncate font-mono text-[#e8e8e8]">edit_file</span>
                <ToolStatus kind="done" />
                <span className="flex-1" />
                <span className="flex items-center gap-2 font-mono text-[11.5px]">
                  <span className="max-w-[180px] truncate text-[#6e6e6e]">
                    src/loop.rs
                  </span>
                  <span className="font-semibold text-[#4ade80]">+2</span>
                  <span className="font-semibold text-[#f87171]">-1</span>
                </span>
                <ChevronRight size={13} strokeWidth={1.75} className="shrink-0 rotate-90 text-[#6e6e6e]" />
              </div>

              <div className="ml-[7px] mt-1 border-l-2 border-[#2c2c2c] pb-1.5 pl-3.5">
                <div className="overflow-hidden rounded-[10px] border border-[#262626] bg-[#131313]">
                  <div className="flex items-center gap-2 border-b border-[#222222] bg-[#161616] px-[11px] py-[7px] font-mono text-[11.5px] text-[#a3a3a3]">
                    <span className="truncate text-[#c9c9c9]">
                      {t("loop.demo.diff.file")}
                    </span>
                    <span className="font-semibold text-[#4ade80]">+2</span>
                    <span className="font-semibold text-[#f87171]">-1</span>
                    <span className="flex-1" />
                    <span className="text-[#5e5e5e]">{t("win.hunk")}</span>
                  </div>
                  <div>
                    {DIFF_LINES.map((line, i) => (
                      <div
                        key={i}
                        className={`flex items-baseline font-mono text-[11.5px] leading-[1.68] ${
                          line.kind === "add"
                            ? "bg-[#4ade80]/[0.07] text-[#a9e8c0]"
                            : line.kind === "del"
                              ? "bg-[#f87171]/[0.06] text-[#dca9a4]"
                              : line.kind === "hunk"
                                ? "bg-[#181818] py-0.5 text-[10.5px] text-[#7aa2f7]"
                                : "text-[#8a8a8a]"
                        }`}
                      >
                        <span className="w-[36px] flex-none select-none pr-2 text-right text-[10.5px] text-[#4d4d4d]">
                          {line.ln}
                        </span>
                        <span
                          className={`w-[16px] flex-none select-none text-center ${
                            line.kind === "add"
                              ? "text-[#4ade80]"
                              : line.kind === "del"
                                ? "text-[#f87171]"
                                : "text-[#6e6e6e]"
                          }`}
                        >
                          {line.kind === "add"
                            ? "+"
                            : line.kind === "del"
                              ? "-"
                              : " "}
                        </span>
                        <span className="min-w-0 flex-1 whitespace-pre pr-3">
                          {line.code}
                        </span>
                      </div>
                    ))}
                  </div>
                </div>
              </div>
            </div>

            <p className="py-0.5 text-[13.5px] leading-relaxed text-[#e8e8e8]">
              {t("loop.demo.done")}
            </p>

            {/* run status line */}
            <div className="flex flex-wrap items-center gap-3.5 px-1 pt-1 font-mono text-[11px] tabular-nums text-[#6e6e6e]">
              <span className="inline-flex items-center gap-1.5 text-[#4ade80]">
                <span className="soyo-pulse-dot h-1.5 w-1.5 rounded-full bg-[#4ade80]" />
                {t("win.streaming")}
              </span>
              <span>{t("win.summary")}</span>
              <span>{t("win.usage")}</span>
            </div>
          </div>

          {/* docked composer */}
          <div className="px-4 pb-3.5 pt-2 sm:px-6">
            <div className="rounded-[14px] border border-[#2b2b2b] bg-[#1d1d1d] px-[14px] py-[12px] shadow-[0_8px_30px_rgba(0,0,0,0.35)]">
              <p className="text-[13.5px] leading-[1.45] text-[#6e6e6e]">
                {t("win.composer")}
              </p>
              <div className="mt-2.5 flex items-center justify-between gap-2">
                <div className="flex items-center gap-1.5">
                  <span className="inline-flex h-[26px] w-[26px] items-center justify-center rounded-[8px] text-[#a3a3a3]">
                    <Plus size={13} strokeWidth={1.75} />
                  </span>
                  <span className="hidden h-[26px] items-center gap-1 rounded-[8px] px-2 text-[12px] text-[#a3a3a3] sm:inline-flex">
                    <Paperclip size={13} strokeWidth={1.75} />
                    {t("win.attach")}
                  </span>
                  <span className="hidden h-[26px] items-center gap-1.5 rounded-[8px] border border-[#2e2e2e] bg-[#1c1c1c] px-2.5 text-[12px] text-[#a3a3a3] md:inline-flex">
                    <Globe size={11} strokeWidth={1.75} className="text-[#8a8a8a]" />
                    {t("win.websearch")}
                  </span>
                  <span className="hidden h-[26px] w-[26px] items-center justify-center rounded-[8px] border border-[#2e2e2e] bg-[#1c1c1c] text-[#a3a3a3] md:inline-flex">
                    <List size={13} strokeWidth={1.75} />
                  </span>
                </div>

                <div className="flex items-center gap-1">
                  <span className="hidden h-[26px] items-center gap-1.5 rounded-[7px] px-2 text-[11.5px] text-[#c9c9c9] sm:inline-flex">
                    <span className="h-1.5 w-1.5 rounded-full bg-[#4ade80]" />
                    CODE
                  </span>
                  <span className="hidden h-[26px] items-center rounded-[7px] px-2 text-[11.5px] text-[#8aa8d8] sm:inline-flex">
                    auto
                  </span>
                  <span className="inline-flex h-[26px] max-w-[110px] items-center rounded-[7px] px-2 text-[11.5px] text-[#a3a3a3]">
                    <span className="truncate whitespace-nowrap">
                      {t("win.model")}
                    </span>
                  </span>
                  <span className="hidden h-[26px] items-center gap-1.5 whitespace-nowrap rounded-[7px] border border-[#2e2e2e] bg-[#1c1c1c] px-[9px] text-[11px] text-[#c9c9c9] 2xl:inline-flex">
                    <Gauge size={11} strokeWidth={1.75} className="shrink-0 text-[#8a8a8a]" />
                    {t("win.effort")}
                    <ChevronUp size={11} strokeWidth={1.75} className="shrink-0 opacity-60" />
                  </span>
                  <span className="hidden h-[26px] items-center gap-1 whitespace-nowrap rounded-[7px] border border-[#2e2e2e] bg-[#1c1c1c] px-2 text-[11px] text-[#c9c9c9] lg:inline-flex">
                    {t("win.steer")}
                    <ChevronDown size={11} strokeWidth={1.75} className="text-[#6e6e6e]" />
                  </span>
                  {/* ContextRing — 19px usage ring */}
                  <svg
                    width="19"
                    height="19"
                    viewBox="0 0 19 19"
                    className="mx-0.5 hidden shrink-0 md:block"
                    aria-hidden="true"
                  >
                    <circle
                      cx="9.5"
                      cy="9.5"
                      r="7.5"
                      fill="none"
                      stroke="#2e2e2e"
                      strokeWidth="2"
                    />
                    <circle
                      cx="9.5"
                      cy="9.5"
                      r="7.5"
                      fill="none"
                      stroke="#d9a08e"
                      strokeWidth="2"
                      strokeLinecap="round"
                      strokeDasharray="47.1"
                      strokeDashoffset="30.1"
                      transform="rotate(-90 9.5 9.5)"
                    />
                  </svg>
                  <span className="ml-0.5 grid h-[28px] w-[28px] shrink-0 place-items-center rounded-full bg-[#d9a08e] text-[#1c120e]">
                    <Square size={10} strokeWidth={2.3} className="fill-current" />
                  </span>
                </div>
              </div>
            </div>
            <div className="mt-2 hidden items-center justify-center gap-4 text-[11px] text-[#5e5e5e] sm:flex">
              <span>{t("win.hint.enter")}</span>
              <span>{t("win.hint.newline")}</span>
              <span className="hidden md:inline">{t("win.hint.tab")}</span>
              <span>{t("win.hint.esc")}</span>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
