# N5 Browser 实现与验收

**2026-11-17：DEV-079 至 DEV-083 已完成功能交付、定制 Chromium ARM64 捕获引擎修复（Stage 5A：23 文件补丁 `df4c0fb6…`，ELF `501,816,112 B` `d3c691a5…`）、本机／专用 Colima VM／独立 Linux ARM64 Masked 截图实测（Stage 5B），以及固定源码 `5fff1c46ba1d07bf5af993745f4bdeb1e90eeb7d`（`5fff1c4`）上的四平台远端终验（Stage 5C）：[CI 44044030536](https://github.com/Ariakage/saya-harness/actions/runs/44044030536)、[N5 native acceptance 44044034857](https://github.com/Ariakage/saya-harness/actions/runs/44044034857)（4/4 passed，0 unsatisfied）与 [Release candidates 44044045558](https://github.com/Ariakage/saya-harness/actions/runs/44044045558)（4/4 passed）均已全绿通过，四组构建产物的 candidate 与 tarball 解包二进制 SHA-256 逐字节一致。`version = 0.2.0` 不变、未发布新发行标签；非 `linux-arm64` 目标架构定制 `headless_shell` 构建分发仍作为显式未覆盖边界保留。**

任务范围来自[后续开发计划](post-mvp-plan.md#n5-browser)；实现与安全契约见 [Browser](browser.md)，对外范围见[中英双语说明](releases/release-notes-n5.md)。

## 提交与范围

| 阶段 | 提交 | 已有实现与证据 |
| --- | --- | --- |
| DEV-079 | `5a61152` | BrowserProvider／BrowserState、受控 Web、操作描述、引用、权限与取消组件 |
| DEV-080 | `058bb9e` | 固定 worker、身份校验、生产装配、专用 VM、双层 sandbox 与生命周期 |
| DEV-081 网络阶段 | `e2587fe` | 共享 Run 网络预算、broker、反向 SSH、私有 netns、旁路 canary |
| DEV-081 凭据／文件／后台 | `f1bbc5c` | origin 凭据槽、下载／上传 artifact、GC／lease、取消与 unknown |
| DEV-082 | `1443291` | CLI／TUI 审批与产物、三协议图片、有效上下文媒体加载、来源保真与导出 |
| DEV-083（Stage 5A／5B） | 本机／VM 实测 | 定制 Chromium ARM64 捕获引擎、`saya/browser/captureScreenshot`、strict `--require-screenshot` 22/22 passed 与 `n5arm22` `1280x720` 原生 Masked PNG |
| DEV-083（Stage 5C） | `5fff1c4` | 四平台远端 `CI`（`44044030536`）、`N5 native acceptance`（`44044034857`）与 `Release candidates`（`44044045558`）全部 4/4 通过，三补丁 worker `de88c9fc…`、Linux x64/ARM64 Chromium sandbox 与 candidate/RC 产物核验一致 |

## 四平台固定源码远端终验（`5fff1c4`）

2026-11-17，在推送修复（含 macOS PTY 背压与终止退避、`vm.rs` 早退上传 `BrokenPipe` 竞争、TUI 选区渲染等待及 Darwin XNU 进程组回收 `EPERM` 边界）后，以固定 Clean 源码 `5fff1c46ba1d07bf5af993745f4bdeb1e90eeb7d` 执行三组四平台远端工作流并全部通过；已下载全部 `n5-acceptance-*` 与 `saya-*` 产物完成独立摘要与日志核验：

| 平台／目标 | Runner 与系统基线 | Rust / Python | Candidate 与 RC 二进制（`saya`） | N5 原生／Browser 关键实测 |
| --- | --- | --- | --- | --- |
| `ubuntu-22.04`（`linux-x86_64`） | Ubuntu 22.04.5 LTS，glibc 2.35，Linux `6.8.0-1070-azure` | Rust 72 suites / **1730 passed**（0 failed，35 ignored）；Python 12 suites / **172 passed** | `72,519,776 B`，SHA-256 `a3e640eed750770875d9349a8cf56a41722bf14aeeefa47de905dc2a404634ed`（`.tar.gz` `2cb8a04f…`）与 RC 解包完全一致 | `workflow-outcomes` 22/22；原生隔离 19/19、性能 21/21、插件 8/8、N3 CLI 34/34、N3 TUI 15/15、TUI 28/28；三补丁 worker `de88c9fc…`（9/15/13 passed）；`browser_vm_host_linux` 1/1；schema v2 manifest `1d914e12…` ＋ Playwright rev 1254 原生 Chromium sandbox `passed`（115.30s，`renderer_pid=73 no_new_privs=true seccomp=2 stderr_initialize_sandbox=true`） |
| `ubuntu-22.04-arm`（`linux-aarch64`） | Ubuntu 22.04.5 LTS，glibc 2.35，Linux `6.8.0-1070-azure` | Rust 72 suites / **1730 passed**（0 failed，35 ignored）；Python 12 suites / **172 passed** | `66,548,720 B`，SHA-256 `09a13baeb1a8591aeb3d4589ffddbbd0fee1c2be98b0a8c866dc18b0a48d683b`（`.tar.gz` `28c4ce66…`）与 RC 解包完全一致 | `workflow-outcomes` 22/22；原生隔离 19/19、性能 21/21、插件 8/8、N3 CLI 34/34、N3 TUI 15/15、TUI 28/28；三补丁 worker `de88c9fc…`（9/15/13 passed）；`browser_vm_host_linux` 1/1；schema v2 manifest `dd82993c…` ＋ Playwright rev 1254 原生 Chromium sandbox `passed`（49.42s，`renderer_pid=73 no_new_privs=true seccomp=2 stderr_initialize_sandbox=true`） |
| `macos-15`（`macos-aarch64`） | macOS 15.8.2 arm64，Darwin `24.6.0`，`minos 15.0` | Rust 72 suites / **1715 passed**（0 failed，27 ignored）；Python 12 suites / **172 passed** | `60,582,784 B`，SHA-256 `8b67b35bd4c9aa0698938f9c97963d6dd06d38633d59bf2726dca050ce6e80e1`（`.tar.gz` `961bbc6b…`）与 RC 解包完全一致 | `workflow-outcomes` 19/19；Seatbelt 原生隔离 18 passed / 1 `not_run`（无 Linux delegated cgroup）、性能 21/21、插件 8/8、N3 CLI 34/34、N3 TUI 15/15、TUI 28/28；三补丁 worker `de88c9fc…`（9/15/13 passed）；Seatbelt 原生 Browser fail-closed 契约保持 |
| `macos-15-intel`（`macos-x86_64`） | macOS 15.7.9 x86_64，Darwin `24.6.0`，`minos 15.0` | Rust 72 suites / **1715 passed**（0 failed，27 ignored）；Python 12 suites / **172 passed** | `64,133,976 B`，SHA-256 `fa525f8e1136ec77e76caa4978bf23d55c7f5f0c870698f1883e4ccdcfda1824`（`.tar.gz` `febef730…`）与 RC 解包完全一致 | `workflow-outcomes` 19/19；Seatbelt 原生隔离 18 passed / 1 `not_run`（无 Linux delegated cgroup）、性能 21/21、插件 8/8、N3 CLI 34/34、N3 TUI 15/15、TUI 28/28；三补丁 worker `de88c9fc…`（9/15/13 passed）；Seatbelt 原生 Browser fail-closed 契约保持 |

- **工作流记录**：[CI 44044030536](https://github.com/Ariakage/saya-harness/actions/runs/44044030536)（14m22s–40m36s）、[N5 native acceptance 44044034857](https://github.com/Ariakage/saya-harness/actions/runs/44044034857)（19m54s–56m16s）、[Release candidates 44044045558](https://github.com/Ariakage/saya-harness/actions/runs/44044045558)（18m29s–36m8s）；四个目标均使用统一工具链 `rustc 1.99.1 (58cf8f9c3 2026-10-13)`。
- **能力边界说明**：远端 `N5 native acceptance` 在 Linux 双架构使用 Playwright rev `1254` 的 stock Chromium（证明安装树 v2 核验、`browser_vm_host_linux` 通道、Bubblewrap/Seccomp 嵌套隔离与 Chromium 内层 sandbox 成立）；定制预捕获 Masked PNG 捕获引擎当前仅针对 `linux-arm64` 完成编译交付并在下节 `colima-n5cap`／`n5arm22` 取得实跑通过（遇未修补引擎继续按设计 fail-closed 返回 `browser_capability_disabled`）。

## 定制捕获引擎修复、Stage 5B 接线与 strict 截图验收

2026-11-17，在保留旧草案 `capture-draft.patch`（`18dc7e9e…`，`draft-capture-red-262.json` 6 passed、5 failed）原始失败记录的基础上，完成全部五处引擎修复与 Stage 5B 生产链路实跑，总汇总见 `target/n5/cap1/evidence/stage5b-complete-265.json`：

- **Stage 5A（5 项引擎修复、打包与 VM 11/11 GREEN）**：修复 `box_fragment_painter.cc`、`web_local_frame_impl.cc`、`args.gn`（`use_dummy_lastchange = false` 输出 `@6449d9f9a6a906edff724b7b220199b3fde4615c`）及 `saya_capture_observer.{h,cc}`（`kPostedMessage` 投递与 `mojo::WrapCallbackWithDefaultInvokeIfNotRun`）。封存 `capture-fixed.patch`（64,546 B，`df4c0fb6…`，`patch-identity-263.json` `53db522d…`），增量构建 Linux ARM64 `headless_shell` ELF（501,816,112 B，`d3c691a5…`，`capture-build-263.json`），经 `package_fixed_capture_runtime_264.py` 闭合打包（`capture-runtime-264.json`），并在 `colima-n5cap`（`saya-capture-fixed-264`，`108dc2e8…`）实测 `browser_screenshot_real` **11 passed / 0 failed**（`212.44s`，`fixed-capture-green-264.json`）。
- **Stage 5B（受限 Worker、Rust Host/VM 接线与全量实测）**：按顺序应用第三份补丁 `scripts/n5/patches/playwright-masked-capture.patch`（9,623 B，`984ac1e6…`）产出 `coreBundle.js`（3,640,550 B，`de88c9fc…`），在 `crates/saya-core/src/{mcp.rs,browser.rs,browser/vm.rs,browser/worker.rs,browser/worker/tests.rs}` 接入 `saya/browser/captureScreenshot`、`MAX_WORKER_SCREENSHOT_WIRE_BYTES = 13 MiB`、Base64/PNG/`IHDR`/masking/URL-origin-staleness 严格核验及 sandbox 失败清理，并在 `browser_vm_files_real.rs`／`browser_screenshot_real.rs`／`browser_worker_real.rs` 加入生产 Masked PNG 与凭据填充像素核验。

| 检查环境／入口 | 实际结果与证据 |
| --- | --- |
| macOS ARM64 `just check`（`minos 15.0`） | 72 suites／**1714 passed、0 failed、27 ignored**，`fmt`、严格 Clippy 与 `--release` 通过（`target/release/saya` 60,578,640 B，`1b752d59c4141790060f64ea2b66ff8ed102814be4759c546416bb5151eee6bd`），日志 `target/n5/cap1/logs/macos-just-check-final.log` |
| macOS 工具与 Smoke／Worker 协议 | `just test-tooling` 11 suites／168 passed；`test-n5-worker-protocol` 9/9、`test-n5-worker-process-info` 15/15、`test-n5-worker-capture` 13/13；`just smoke target/release/saya` 全部通过（含 R-01..R-05 与 28 组 TUI PTY），日志 `target/n5/cap1/logs/macos-{tooling,worker-protocol,worker-process-info,worker-capture}-265.log`、`macos-smoke-final.log` |
| `colima-n5cap`（`saya-capture-fixed-265`，`da8e5409…`）VM 套件 | `browser_screenshot_real` **12/12 passed**（138.09s，含生产 `PlaywrightMcpProvider` 凭据填充与像素遮蔽）、`browser_vm_real` 1/1（11.64s，`renderer_pid=56381/56493 no_new_privs=true seccomp=2`）、`browser_vm_files_real` 1/1（120.86s，确认 Masked PNG 产物且 `alice-secret` 不落盘）、`browser_vm_network_real` 1/1（22.72s，`observations=6`），日志 `target/n5/cap1/logs/final-vm-*-265.log` |
| `colima-n5cap` Strict 联合验收（`--require-screenshot`） | `python3 -I -B scripts/n5/acceptance.py target/release/saya --require-screenshot`：**22/22 passed、0 failed、0 disabled**（Execution 140、Review 8、Compact 1，`real-screenshot` passed 并将捕获产物注入后续多模态请求，远端 Run 根及进程均空），报告 `target/n5/cap1/acceptance-265-final/run-23_y48z2/evidence.json` |
| Linux ARM64 `n5arm22`（`/home/saya-test/native-capture-265`，`8a2a06f0…`） | `just check` 72 suites／**1729 passed、0 failed、35 ignored**（Linux `saya` 66,631,008 B，`f0d15cc6c54764549760164f860bcc6d60267b3718fcacdf3828e32d57d5c73c`）；`just test-tooling` 11 suites／167 passed／1 平台 skip；三项 `test-n5-worker-*`（9/15/13 passed）；`just smoke` 全过；`browser_worker_real` **2/2 passed**（79.63s，覆盖 `1280x720` 原生 Bubblewrap/Seccomp Masked PNG 捕获，`renderer_pid=74/76 no_new_privs=true seccomp=2 stderr_initialize_sandbox=true`），零残余进程，日志 `target/n5/cap1/logs/n5arm22-full-265.log` |

此前 `task-2494` 中断遗留的单个远端 dir `saya-browser-vm-85893b2396778cb8` 经核对时间戳与进程为空后手动清理，随后在干净状态完成 `run-23_y48z2` 及四套 VM 套件复跑全过。Linux x86-64／macOS Intel 当前源码、非 `linux-arm64` 定制引擎构建分发及远端四平台 CI 仍待外部平台环境执行，不据此提前声称全平台发行完成。

## 安装树身份 v2 本机阶段

schema v2 的规范、资源上限与迁移要求见 [安装树身份](browser.md#安装树身份-v2)。manifest 继续逐项绑定七个命名文件，新增三棵完整安装树；未命名依赖或资源的增删改、权限与链接变化也必须拒绝。链接不跟随扫描，按树内成员图解析；常规文件在同一已打开 FD 上双读内容，不能以同一时钟 tick 内可能不变的 stat 代替内容一致性。双读不是原子快照，安装需由宿主管理并在校验及启动期间保持不变；它不认证源码／DEPS／工具链／构建配方。

第 176／168 轮门禁之后，第 179 轮在独立 user／mount namespace 中真实复现 Rust 接受跨设备 symlink，而 Node／Python 拒绝；第 181 轮持久回归在正确拒绝断言上 RED，最小设备检查修复后第 182 轮七项定向回归全部 GREEN。修复后的 macOS 第 184 轮与 Linux 第 187 轮完整门禁已通过；Linux 第 185 轮失败与第 186 轮同程序复验分别保留，chat 超时根因仍未确认。第 190 轮另行完成 Rust 生产 VM 身份通道的正控、安装树拒绝与恢复。下表明确区分修复前历史与修复后输入，不迁移旧成功。证据位于 `target/n5/linux-full-20261115/`：

| 检查 | 实际结果 | 证据 |
| --- | --- | --- |
| macOS 完整 Rust 门禁（修复后） | 第 184 轮 72 组结果，1686 passed、0 failed、25 ignored；格式、全目标严格 Clippy、标准 release 通过，同新程序 smoke 通过 | `evidence/identity-workspace-macos-184.json`、`logs/identity-macos-184-check.log` |
| Linux ARM 完整 Rust 门禁（修复前历史） | 第 168 轮 1701 passed、0 failed、22 ignored；格式、全目标严格 Clippy、标准 release 通过 | `evidence/identity-workspace-168.json`、`data/identity-workspace-168/check.{stdout,stderr}` |
| Linux ARM 完整门禁（修复后） | 第 187 轮 72 组结果，1701 passed、0 failed、23 ignored；格式、全目标严格 Clippy、标准 release 通过，不覆盖或重写源码，历史与输入守卫通过 | `evidence/identity-workspace-187.json`、`data/identity-workspace-187/check.{stdout,stderr}` |
| Linux ARM 最终复验失败轮 | 第 185 轮格式与严格 Clippy 通过；workspace 测试 1700 passed、1 failed、23 ignored，chat 等待未保存结果超时；后续 release、显式探针、工具和 smoke 未运行 | `evidence/identity-workspace-185.json`、`data/identity-workspace-185/check.{stdout,stderr}` |
| Linux 同程序 chat 复验 | 第 186 轮不重编、不修改原断言及 8 秒超时：精确用例 1 passed／0.87 秒，完整套件 21 passed／14.30 秒；程序与输入未变，所属进程清理通过。未再次复现，根因未确认 | `evidence/chat-recheck-186.json`、`data/chat-recheck-186/{exact,chat-suite}.{stdout,stderr}` |
| Linux 固定 Node 探针与跨设备回归（修复后） | 第 187 轮显式实跑共 6 项：五项 Node 探针及独立 namespace 持久跨设备回归全部通过、0 ignored | `data/identity-workspace-187/node-probe.{stdout,stderr}` |
| 完整 Python 工具回归（修复后） | macOS 第 184 轮 168 passed；Linux 第 187 轮 167 passed、1 skipped。两端均实际运行 11 个 suite、168 个用例 | `logs/identity-macos-184-tooling.log`、`data/identity-workspace-187/tooling.{stdout,stderr}` |
| 同程序 smoke（修复后） | macOS 第 184 轮与 Linux 第 187 轮各自的新 release 分别通过离线 PLAN、会话 journey、五项审查、28 组真实 TUI 旅程；前后程序身份一致 | `logs/identity-macos-184-smoke.log`、`data/identity-workspace-187/smoke.{stdout,stderr}` |
| 修正后的定向复跑 | Rust 改写一项与 Node 五项全部通过；原源码／程序守卫不变，所属进程组已回收 | `evidence/identity-mutation-tests-175.json` |
| 跨设备持久 RED／GREEN | 第 181 轮编译成功、跨设备拒绝断言失败；最小修复后，第 182 轮该项、Rust 改写与五项 Node 探针全部实跑通过，旧失败程序独立备份 | `evidence/identity-cross-device-tests-{181,182}.json`、`data/identity-cross-device-tests-{181,182}/` |
| Rust 生产 VM 身份通道 | 第 190 轮在专用 `n5cap` 的全新根正常 provision／check；实际 Rust verifier 正控通过，添加未声明常规文件后在安装树边界以已知 `worker_identity` 拒绝，删除该自有文件后恢复通过；未启动 Browser | `data/identity-vm-190/evidence/report.json`、同目录三份 `rust-{positive,negative,restored}.stdout`（带步骤前缀） |

Linux 的平台 skip 仅因为系统不能为符号链接设置非 `0777` 模式，不把它计为实跑；macOS 实际覆盖该模式规范化边界。真实 stat 改写记录的读前／读后 dev、ino、size、mode、nlink、mtime、ctime 完全相同，真实内容已变，修复后仍拒绝。另一个同 tick 用例仅归一化目标 inode 的时间观察，其余 I/O 与摘要真实；它与真实平台 stat 用例分别记录，不互相冒充。

保留的失败与修正顺序：

- 第 157 轮 Rust 在真实元数据不变的改写后错误接受；同 FD 双读后，第 162／163／175 轮拒绝。第 162 轮旧 Node 在真实 stat 与独立同 tick 观察两项上错误接受；双读后第 163／175 轮拒绝。原始 stdout／stderr 和改写 audit 随各轮保留。
- 第 171 轮 Rust 接受合法 BOM 名称／链接，而旧 Node 探针失败；`TextDecoder` 保留 BOM 后，第 172 轮探针已成功，但测试比较了重新密封前的旧 13 项 fixture，实际树为 15 项。该轮仍记录失败，不计 GREEN。测试改为读取重新密封的磁盘 manifest 后，第 175／168 轮完整相等检查通过。
- 第 179 轮以真实 `open_tree`／`move_mount` 建立跨设备 symlink：目标仍为树内合法文件、nlink 为 1，读前后元数据稳定，Rust 接受而 Node／Python 拒绝。第 181 轮新 libtest 编译成功，并在 `a foreign-device installation symlink must be rejected` 断言失败；不是 namespace、挂载或编译错误。链接分支补齐 device 检查后，第 182 轮同一持久测试、Rust 改写与五项 Node 探针全部通过；新旧 libtest 摘要分别为 `2820e42a8ad3ba342ff649726bced7fe2bb0d314219e77efd0c26c4b0b6a9d93`、`36743220a7852ee6d84ee1af9c0f61810a515b80213de983a119f5693d0ef87b`。旧实现与失败程序在第 182 轮独立 inode 备份，原报告／日志不变；第 181 轮私有源码根从第 182 轮起受控升级，不能再把原路径当作永远冻结的旧程序。成功父测试不转印子进程 stderr，GREEN 依据已绑定测试中实际执行的 uid／namespace／device／正控／拒绝断言，不推断未输出的新 namespace 或 device 数值。
- 第 188 轮 Cargo 构建退出 0，但同一 `serde_json` 的运行时与 host／build profile 形成两个合法产物，旧夹具错误地要求全局唯一。该轮在解析阶段失败，未编译 driver、provision 或执行 Rust 校验，原根前后均不存在；输入、VM 配置与清理守卫通过。第 190 轮从实际 Cargo JSON 中由唯一 core 确定完整 profile，再唯一选择同 profile 的依赖；同 profile 重复及失败的 build-finished 负控仍拒绝，不按 glob、修改时间或首项选择。该修正不改变生产 Rust 或身份断言。
- Python 的同 FD 复读与单 Node 私有快照回归分别留在 `logs/python-stability-150-*`；首次 umask 导致的权限前提失败不算行为 RED。

第 190 轮真实 Rust 通道使用已核验的 macOS 第 184 轮 CLI 作为宿主 launcher，经生产 `SshVmCommandRunner`／`VmWorkerIdentityVerifier` 以及 stdin 传入的固定 Node 探针完成三次校验；不是 Node 直跑或 Python check 的替代说法。schema v2 manifest SHA-256 为 `c67a57d3dc94d774e28482bf1cf818500638eccf99de92d51765020466773764`，报告 SHA-256 为 `8fe62da841f3112e902434b79bb3c9aa725b2f625986a5b02e13807eb43c4a68`。三树计数／常规字节分别为 Node 2／122,295,568、worker 233／19,160,460、Chromium 290／296,457,671。新远端根为 `/home/ariakage.guest/saya-identity-v2-190`；只添加和删除这一根下的固定负控文件，旧根不覆盖，VM 不重启、不改全局工具或 sysctl。源输入、素材、VM 启动进程／SSH 身份与资源约束前后不变；全部所属进程组已回收，远端 Run 根及关联进程、宿主临时项与关联 SSH 进程为空，未使用 SIGKILL。该轮只验安装身份，不验 Browser 启动、stdio 生命周期或捕获。

macOS 第 184 轮程序为 `target/n5/capture-rust-20261115/release/saya`，60,418,800 字节，SHA-256 `28cb78cac5143d9e84cf2b7cc394fe8ef3cbd48269868451fd06613295fd887c`，部署目标 15.0／SDK 27.0。第 176 轮旧 `7fa2eeeacb83154c58b6a458e0c77e9cbe502ef1a1174f99d760d74f45aadda6` 程序已独立备份于 `data/identity-macos-184/saya-before-184`。Linux 第 187 轮程序为 `/workspace/target/release/saya`，66,589,720 字节，SHA-256 `550445a6d7f225e2e756d17935ee7699fbac93e70ac482605080f4aa9b36db4b`；release rlib 为 `ef691de2424285f41dd5a2d0040afd312e7f071c3513b4b36b293ae74ba986d3`。第 168 轮旧 `36549c0fb85c7ee8c8fd039754d43cf059ef65d3e1286a43b0478ac3bb168a10` 程序和旧 rlib 已分别保存在 `/workspace/target/n5/identity-workspace-187/saya-before` 与 `rlib-before`，是独立 inode 副本；不追改第 185 轮未进入 release 构建的记录。旧 138 程序继续保存在第 168 轮 `saya-before`，摘要为 `d0d44170…`。

Linux 的来源是 `557f865` 加各轮列明八文件及保留的 `execution_backend.rs` 补丁，不是宿主 HEAD 全树；第 185 轮已将八文件升级至跨设备修复输入，第 187 轮保持这些源码字节不变并重建 release。macOS 第 184 轮报告绑定当时 HEAD 和列明的未提交输入，包含未完成的截图草案测试。默认 ignored 与编译成功都不计为真实截图通过。旧 138 的文件／程序身份已由 168 的受控备份和新构建取代，后续不能再用旧守卫脚本声称新程序不变。原临时目录的 23 项基线全部保留，168、185 与 187 各自临时目录的残留分别记录；第 187 轮有四项自有残留，不宣称所有临时项为空。

本节确认安装身份 v2 的本机阶段及明确的验证范围：v1 拒绝、provision 新根、完整安装树正负控和恢复均有实证；它不完成受限 stdio worker、native managed provider、生产捕获事务或新增四平台。`screenshots`／`screenshot_masking` 继续为 false，未推送、发布或运行新增远端 CI。下文较早程序及 v1 VM 记录按各自来源保留，不能用于认证当前 v2 运行时。

## 受限 worker 补丁与私有协议阶段

2026-11-16，本阶段仅交付[受限 stdio 与固定角色查询补丁](browser.md#受限-stdio-worker-补丁)、显式协议回归和补丁组合的 VM 生命周期，不把新私有角色回复当成已经接入的 native kernel 证明。生产 Rust、launcher、网络／凭据／PLAN 策略与截图 facts 未改；完整 Rust 门禁未重跑，已有 macOS 第 184 轮与 Linux 第 187 轮结果仍按其原输入保留。

固定包版本不变。两个补丁依次应用于 stock 独立副本，最终仅 `node_modules/playwright-core/lib/coreBundle.js` 改变，232 个非根成员的内容、权限与链接均与候选逐项核对。stock、第一补丁后与最终 coreBundle SHA-256 分别为 `9d3260b17408501ba46ca708fa870a7c95e1e3f6e9cc34d799465f17b7d78c50`、`385866007a5df69db4c3e22bea193b2afe7ae20b54769a144855b52e1d608a0d`、`28512f80e0a84cb225bd1431217f0ca98265d5010cf920c2f171e28fc056093a`。补丁组合不绕过身份树校验或复用旧 manifest。

以下路径相对 `target/n5/linux-full-20261115/`：

| 检查 | 实际结果与边界 | 证据 |
| --- | --- | --- |
| 缺失私有接口 RED | 第 194 轮合法空／非空对象收到 `-32601`；第 201 轮组件 12 项在同一缺失方法边界失败、未启用 factory 的第 13 项通过。第 194 轮另有三种非法 envelope 被上游丢弃后的超时，不算产品 RED | `evidence/worker-protocol-194.json`、`evidence/worker-process-info-201.json` |
| 取消 RED／GREEN | 第 206 轮原 13 项通过，新增项在 CDP 响应仍挂起、取消后未 detach 的断言失败；仅转交 signal 后，第 207 轮 14 项全部通过 | `evidence/worker-cancel-{206,207}.json`、`data/worker-cancel-{206,207}/stdout` |
| 真实 SDK 元数据与兼容性 RED／GREEN | 第 214 轮真实 Rust SDK 在空业务参数中发出 `_meta.progressToken=0`，恰好一个私有请求；第 215 轮组件新增项及四个 CLI metadata 子例实际收到 `-32602`。严格空业务参数 schema 修复后，第 216 轮组件 15 项、CLI 9 项全部通过 | `evidence/rmcp-process-info-wire-214.json`、`evidence/worker-metadata-{215,216}.json` |
| 最终完整工具与持久回归 | 第 218 轮 `just test-tooling` 11 个 suite、168 passed；两个显式 Just 入口分别 9 项真实 CLI 通道和 15 项组件全部通过。宿主 Node 为 `v22.23.0`，不将其称为固定 Linux runtime 或 native 平台验收 | `evidence/worker-checks-218.json`、`logs/worker-{tooling,protocol,process-info}-218.log` |
| 标准检查掩码对照 | 第 209 轮继承 `077` 后，golden tree、link mode 与 provision verifier 三项失败；第 210 轮诊断驱动参数错误，未执行对照。第 211 轮同源码三项在 `077` 下失败、`022` 下通过；第 212／218 轮按开发指南对子进程设置 `022` 后完整通过，未改断言或安装策略 | `evidence/worker-checks-209.json`、`data/worker-umask-{210,211}/`、`evidence/worker-checks-{212,218}.json` |
| 真实初始页失败及保留 | 第 195 轮真实生命周期入口 exit 101；第 199 轮经生产 launcher／连接读取到原 MCP 错误：89 字节 socket 目录过长。第 197／198 轮确认无关联进程后，仅原子归档自有失败 Run／宿主临时目录，未删文件或发送信号，不算重跑成功 | `evidence/vm-lifecycle-{195,197,198}.json`、`evidence/vm-worker-mcp-199.json` |
| 归档清单前置失败 | 第 208 轮对历史归档 socket 使用安装树扫描器而失败，未 provision 或启动 Browser；第 213 轮专用只读归档清单完成后，旧 `f537…` 候选通过真实生命周期。socket 不打开／连接／删除，安装树的特殊文件拒绝不变；旧成功不转印至最终候选 | `evidence/worker-vm-{208,213}.json`、`data/worker-vm-213-preparation/archive-fixture.json` |
| 最终候选真实 VM 生命周期 | 第 217 轮在新根正常 provision／check，旧 Rust 生产生命周期入口实际 1 passed、0 failed、0 ignored，初始页／close／reopen／snapshot／shutdown 均完成；全部步骤退出 0，无超时或 SIGKILL，前后输入与历史归档相等 | `evidence/worker-vm-217.json`、`data/worker-vm-217/09-libtest.{stdout,stderr}`、`data/worker-vm-217-preparation/replay.json` |

第 217 轮新根为 `/home/ariakage.guest/saya-identity-v2-217`，未缩短路径以规避旧 socket 错误。manifest SHA-256 为 `fd401d1ed8ab07201d10835584bde4a02f973a0a3bfb57249e5036d5572c03a4`，worker 树为 `dea1a1a4115aadc25f36819aeac99b39a8a615825ec512df549f629ed743e98f`／233 项／19,164,091 字节，报告 SHA-256 为 `705f24f6a4fdb46abe58dc981092f083874d66bedfab98ef18d3490286dfa3dd`。宿主 launcher 为 macOS 第 184 轮 `28cb78ca…`，libtest 为 `5be6b520f68cea8b1de5398d5f4136babe56c5c793828f9cbe4d3ba87e03824e`；VM 使用固定 Linux Node `v22.23.5`。实际 renderer PID 10960、重开后 11070 均为 `NoNewPrivs=1`／`Seccomp=2` 且未禁用 sandbox；`stderr_initialize_sandbox=false` 为真实观测值，不把它改为成功断言。宿主与远端关联进程、远端 Run 根、该轮临时目录均为空，旧 190 根的安装与失败归档前后不变。

私有方法未列入模型工具目录，不能接受任意 method／script／selector／processId，HTTP／共享／非 isolated 通道均有实际拒绝回归。组件中的 PID／namespace 为受控字面 fixture，不是新内核采样；第 217 轮也只验证现有 VM 生命周期，尚未在 Rust native inspector 中消费新私有接口、绑定权威角色／namespace／starttime／exe 或证明相对 baseline 的额外 Seccomp。网络／文件联合矩阵、生产捕获与新增四平台未在本阶段重新验收；`screenshots`／`screenshot_masking` 保持 false，N5 全范围未完成，未推送、发布或运行新增远端 CI。

## Rust 固定请求接线

2026-11-16，本阶段为 native／VM 的宿主连接接入同一个固定私有查询，增加严格角色回复校验与取消／超时／future 丢弃边界。`WorkerConnection::process_info` 没有任意方法或工具参数，不调用 `tools/list`／`tools/call` 完成角色查询，也不重放。派发和响应共享请求期限；取消通知最多等待五秒，发送不能确认或 future 被丢弃时使连接失效。现有进程、VM 与目录清理仍独立执行，不把失效连接当成清理已完成。native inspector、原 sandbox 判据、网络／凭据／PLAN 与截图 facts 未改。

以下路径相对 `target/n5/linux-full-20261115/`：

| 检查 | 实际结果与边界 | 证据 |
| --- | --- | --- |
| Rust 固定请求 RED／GREEN | 第 220 轮编译成功后，真实连接在固定查询不可用处失败；接线后第 221 轮固定请求及非法角色回复两项通过 | `logs/worker-rust-query-{red-220,green-221}.log` |
| 在途取消与超时 RED | 第 222 轮请求确实到达后取消，两项在未收到 `notifications/cancelled` 处失败；第 223 轮还复现阻塞通知后连接可复用，3 passed、3 failed | `logs/worker-rust-cancel-red-{222,223}.log` |
| 取消通知与 fixture 收尾 | 第 224 轮通知、超时与连接失效判据通过；刻意停止读取的测试对端导致 shutdown unknown，5 passed、1 failed。后续仅在失效断言之后释放 fixture，不弱化生产判据 | `logs/worker-rust-cancel-green-224.log` |
| future 丢弃 RED／GREEN | 第 225 轮 8 passed，仅已派发 future 被丢弃后仍复用连接处失败；增加失效 guard 后，第 226 轮 9 passed、0 failed、0 ignored。另覆盖预取消、关闭连接、有效但超过字节上限的回复、未知字段／PID／namespace 拒绝及不依赖目录 | `logs/worker-rust-drop-red-225.log`、`logs/worker-rust-query-green-226.log` |
| 完整 Rust 与同程序回归 | 第 227 轮完整 `just check` 为 1695 passed、0 failed、25 ignored；格式、严格 Clippy、release、168 项工具、9 项真实 CLI、15 项组件，以及同一 release 的 smoke／文本旅程／5 项审查／28 组 TUI 全部通过。六个命令均退出 0，进程组消失，源码前后相等；包装报告因五个临时 fixture 留存仍记 failed，不追改 | `evidence/worker-rust-checks-227.json`、`data/worker-rust-checks-227/*.log` |
| 自有 fixture 留存归属 | 第 228 轮用独立保存的旧 core 程序运行插件模块 14 项，复现四个只读 HTTP 插件缓存；另一个目录对应 `execution_backend` 的静态 bootstrap fixture。确认原目录及对照目录均无打开句柄后，仅原子迁移到自有证据目录，成员 inode、模式与内容摘要不变；原报告、源码与程序未改 | `evidence/worker-rust-temp-228.json`、`data/worker-rust-temp-228/` |
| 当前 Rust 的真实 VM 私有查询 | 第 230 轮当前 core／release 经生产身份、environment、launcher 与 `VmWorkerConnection` 完成四次查询及三次固定页面调用；初次导航前、close 后已知拒绝，导航及 reopen 后返回角色。十步均退出 0，无超时、SIGTERM 或 SIGKILL，所属进程组消失 | `evidence/worker-process-info-vm-230.json`、`data/worker-process-info-vm-230/06-private-query-lifecycle.{stdout,stderr}` |

第 227 轮新 release SHA-256 为 `8af6d47c4c94fc6fa6551283ac2dbf3c97a7e46c3a309fad6e76f7202688384a`／60,444,976 字节；旧程序与库在第 219 轮独立保存为 18 个非硬链接副本，不以重新编译替换历史证据。第 230 轮探针为 `207840a86e13deae27fd9c94f8f2e9bd4ba9766001172cd365f7f61c74e20d24`，报告为 `ed2fcde0dd9828e3aee3bf202cdccbab89a8b5292eda630f17fb1b845984f52b`。探针直接链接第 227 轮 Cargo JSON 中与 core 完全相同 profile 的库，另以当前 release 固定宿主 launcher；不复用第 217 轮旧 Rust 程序冒充新接线验证。

VM 继续使用已校验的 v2-217 安装，manifest 为 `fd401d1ed8ab07201d10835584bde4a02f973a0a3bfb57249e5036d5572c03a4`。实际 Node PID 为 11643，PID namespace 为 `dev=4`／`ino=4026531836`；首次 browser／renderer 为 11672／11731，重开后为 11742／11795。这些是固定协议的真实返回值，不是宿主 procfs 联合核验。宿主与远端关联进程、远端 Run 根和该轮临时目录均为空；190 历史安装／失败归档及专用 VM 配置前后不变。

本阶段只闭合 Rust 固定请求与其 VM 生命周期；native 的 namespace／starttime／exe／额外 Seccomp 证明、真实生产捕获和新增四平台仍未完成。ignored 不计为截图成功，`screenshots`／`screenshot_masking` 保持 false，未推送、发布或新增远端 CI。

## 原生角色查询与内核隔离核验

2026-11-17，在上一节本地句柄复核基础上完成 native 浏览器权威角色绑定与强内核隔离核验，三份修改源码为 `crates/saya-core/src/browser/worker.rs`（218,999 B，SHA-256 `98d7f2018182c15f51a8189b6fa868883c763daaa956b5c2d78df75c3545e840`）、`crates/saya-core/src/browser/worker/tests.rs`（166,878 B，SHA-256 `5d715483445938a79e0d78727718228a1667ee51993cc5eee110b650c3676ddc`）与 `crates/saya-core/src/browser/vm.rs`（207,138 B，SHA-256 `e118b70a4aafef9dcfe078ec5e63fc0677f070b622b1211293b6a44c72e47d4b`）；未完成 `crates/saya-core/tests/browser_screenshot_real.rs`（`f2234e18…`）及空 `CONTRIBUTING.md` 保持未提交。

核心变更见 [Browser 架构约束](browser.md#dev-080-worker-与生产装配实现状态)：`ChromiumSandboxInspector::inspect_connection` 统一绑定连接、`NativeSandboxContext<'a>` 与 `capture_linux_node_baseline`，`ProcfsChromiumSandboxInspector::verify_once` 在单一 5 秒期限内执行 `q1`／`q2` 双向角色一致性、`pidfd`／`starttime`／`verify_linux_process_exe`、`NS_GET_PARENT`（1..=8 跳、`EPERM` 失败关闭）／`NS_GET_USERNS`、`NSpid`、`CapEff=0`／`NoNewPrivs=1`／`Seccomp=2`、递增 `Seccomp_filters`（`renderer > browser >= node >= 1`）、Chromium `setproctitle` 空格合并 `/proc/<pid>/cmdline` 归一化（`normalize_cmdline_tokens`＋`has_forbidden_sandbox_flag`）及 `InitializeSandbox` stderr 检查。新增 8 项协议／内核基线单元项、正反 `setproctitle` cmdline 单测、弱 Seccomp 滤器拒绝单测与 `vm_sandbox_inspect_connection_requires_live_pid_and_delegates`。

macOS 与独立 Linux ARM64 `n5arm22`（Ubuntu 22.04，内核 `6.8.0-149-generic`）按同三份源码 SHA-256 验证轨迹如下（Linux 路径相对 `target/n5/linux-full-20261115/`）：

| 检查／轮次 | 实际结果与边界 | 证据 |
| --- | --- | --- |
| macOS 完整门禁 | `RUST_TEST_THREADS=1 just check` 72 组 1710 passed、0 failed、25 ignored；`just test-tooling` 11 suite、168 passed；release `target/n5/capture-rust-20261115/release/saya`（60,291,632 B，`0o755`，SHA-256 `dcb48089aecc28db2162eeeecfbcf3d0913a0549826e745f68041593a902648a`）通过 `just smoke`（含 R-01..R-05 与 28 组 TUI PTY）；macOS 不具备 Linux native Chromium 测试条件 | 会话 `task-1248` 执行日志与产出二进制 |
| Linux ARM 第 253 轮（保留 smoke 失败） | `full-rust-check` 通过（752.78s、exit 0、72 组 1725 passed、0 failed、23 ignored），`tooling` 通过（2.15s、exit 0），`current-cli-smoke` 在瞬态 `tui_journey` 竞争处返回 exit 1；不追改状态 | `data/native-image-gate-253/receipt.json` |
| Linux ARM 第 254 轮（重启基线） | 记录 `n5arm22` 重启后不透明背景进程集合 `{'617', '1178'}`；未改动源码 | `data/native-image-gate-254/receipt.json` |
| Linux ARM 第 255–257 轮（三轮真实故障定位，保留失败） | 先后 reclaim `ni253-real` 并无损归档 `ni253`；第 255 轮暴露测试夹具路径 `linux64` vs `linux-arm64`（exit 101）；第 256 轮暴露 inspector 5 秒期限内 `check_named` 重复读取大可执行 SHA-256 超时（exit 101）；第 257 轮 `full-rust-check` 通过（797.95s、exit 0、1725 passed）、`tooling`／`smoke` 通过，`browser_worker_real` 暴露 Linux Chromium zygote／renderer 调用 `setproctitle` 将 `/proc/<pid>/cmdline` 以空格合并而非 NUL 分隔导致的误判（exit 101） | `data/native-image-gate-{255,256,257}/receipt.json` |
| Linux ARM 第 258 轮（preflight 阻断） | 因 `/home/saya-test/ni257-real` 尚存，`steps: []` 提前阻断并完成 `postguards_complete: true` | `data/native-image-gate-258/receipt.json` |
| Linux ARM 第 259 轮（全量构建与首次实跑通过） | 回收空 `ni257-real` 并无损归档 `ni257` 四项夹具至 `/home/saya-test/ni257-preserved-fixtures-259`；在最终源码上通过 `full-rust-check`（798.58s、exit 0、72 组 1725 passed、0 failed、23 ignored，Linux release `/workspace/target/release/saya` 为 66,585,480 B，SHA-256 `e92c4c778a37b18625600c856e61cc80803ef5611747bf809f78b48e8e27c07f`）、`tooling`（2.14s、exit 0）、`current-cli-smoke`（88.48s、exit 0）及 `browser_worker_real`（46.49s、exit 0，实测 `test real_playwright_worker_verifies_chromium_sandbox ... ok`，stderr 为 `renderer_pid=74 no_new_privs=true seccomp=2 stderr_initialize_sandbox=true`），四步所属进程组均已回收（`empty_known: true`）；仅因单 crate `cargo test -p saya-core --test browser_worker_real` 就地更新 `libsaya_core-652763c80c37cb2d.{rlib,rmeta}` 触发后置产物比对失败而记 `incomplete` | `data/native-image-gate-259/receipt.json`（SHA-256 `531d29cb5625b0d4f9ee6800a1d77a8698673c357c5feb0e4c4da26324d57391`） |
| Linux ARM 第 260 轮（最终封存通过） | 无损归档 `/home/saya-test/ni259` 四项 HTTP 插件只读夹具（`.tmpMHndo4`／`.tmpO2odHa`／`.tmpTSrSj3`／`.tmpZOJbJr`）至 `/home/saya-test/ni259-preserved-fixtures-260` 并回收 `ni259`；复核 70 项 Gate 248 历史归档与 84 项 Gate 259 预备份；衔接 Gate 259 `full-rust-check`（72 组 1725 passed）并在零产物漂移（`programs_before == programs_final`、新增压缩备份 0 B、剩余 3,983,970,304 B）下运行通过 `tooling`（2.16s、exit 0、11 suite／168 用例、167 passed、1 平台 skipped）、`current-cli-smoke`（88.97s、exit 0、smoke／journey／R-01..R-05／28 组 TUI）及 `browser_worker_real`（21.80s、exit 0、`test real_playwright_worker_verifies_chromium_sandbox ... ok` 用时 21.21s、`renderer_pid=74 no_new_privs=true seccomp=2 stderr_initialize_sandbox=true`），全部三步进程组已回收，`tmp_entries_after=[]`、`tmp_real_entries_after=[]`，回执结论为 `status = diagnostic-only-with-global-inventory-unknown`、`rust_gate_complete = true`、`native_browser_verified = true`、`transaction_complete = true` | `data/native-image-gate-260/receipt.json`（SHA-256 `c22d6841c15327f8955383ce8fc9de3335622a996bf37cc55b1fd95a31732af5`）、`data/native-image-gate-260/guest-report.json`（SHA-256 `bfb28d12280d67882451d96006e150c1df20cb1c95bd2f8b8cbc6ac8adc1c553`）、`helpers/{run,test}_native_image_gate_260.py` |

上述门禁封闭 native Linux ARM64 权威角色查询与内核隔离联合核验及 macOS 对应单测／门禁；`screenshots` 与 `screenshot_masking` 维持 `false`，受管捕获引擎部署、真实预捕获遮蔽（`crates/saya-core/tests/browser_screenshot_real.rs`）与新增四平台固定源码发行证据仍未完成，未推送、发布或触发远端 CI。

## 本地可执行句柄与内容复核

2026-11-17，为阻止已通过安装身份校验的 Node／Chromium 可执行文件在规划后至实际启动前发生不改变时间戳的同 tick 改写，本阶段修改三份源码：`crates/saya-core/src/browser/worker.rs`（160,551 B，SHA-256 `4b6df6e98046ad8e8c5cefba6014fb6aef9a7a11f412f54e873ef8b93e0e666a`）、`crates/saya-core/src/browser/worker/tests.rs`（134,242 B，SHA-256 `830ee9f66e2f595adf8cfefcee7d3f85751d907f4e92d1842bd446bf8253d50f`）与 `crates/saya-core/src/browser/vm.rs`（203,676 B，SHA-256 `0a38c43c49093d4010d359331b823c11513c32b8a1a6c9850e8c88ae7153c77e`）。`PinnedWorkerImage` 同时保存已认证 `sha256` 与原始 `File` 描述符；`check_named` 通过 Unix `FileExt::read_at`（64 KiB 分块、512 MiB 上限，逐块检查取消与期限）核对精确大小、EOF 与内容摘要，并保留原有 FD 与重开 `named` FD 的前后完整 `WorkerImageIdentity` 检查；`ManagedProcessLauncher::launch` 通过 `checked_add` 对 Node／Chromium 共享重核期限并保留 `"cancelled"` 取消语义。未完成协作草案 `crates/saya-core/tests/browser_screenshot_real.rs`（`f2234e18…`）及空 `CONTRIBUTING.md` 保持未提交状态。

下表保留本阶段 macOS 验证与 Linux ARM `n5arm22` 从失败到完整通过的全部实证（Linux 路径相对 `target/n5/linux-full-20261115/`）：

| 检查／轮次 | 实际结果与边界 | 证据 |
| --- | --- | --- |
| macOS 完整 Rust＋工具＋smoke | `RUST_TEST_THREADS=1 just check` 1701 passed、0 failed、25 ignored（含 6 项新增／修正复核单测），release ELF `027090788d3046e06409c4d86d6d2dbd8f20e18461793d5bfe90a4d9afdca628`（60,315,632 B，独立备份于 `target/n5/saya-native-image-249-02709078…`）；`just test-tooling` 11 suite、168 passed；同程序 smoke 含 5 项审查与 28 组 TUI 全过 | `target/n5/native-image-stage249-macos.json`、`target/n5/native-image-{check,tooling,smoke}-249-main.log` |
| Linux ARM 第 245 轮（保留失败） | 备份容量 preflight 在覆盖／Cargo 前明确拒绝；源码、程序与基线不变 | `data/native-image-gate-245/receipt.json` |
| Linux ARM 第 248 轮（保留失败）＋第 250 轮夹具归档 | 第 248 轮 `just check` 在 Linux 同 tick 用例 `launch_planner_rejects_backdated_verified_executable_mutations` 失败（1713 passed、1 failed），`/home/saya-test/ni248` 留存 4 项 `runtime_plugins::tests` 只读 HTTP 夹具（65 行成员，原报告保留 `incomplete`）；第 250 轮确认所属进程 64600 进程组为空且获 13 把协作锁后，经 `renameat2(RENAME_NOREPLACE)` 无损迁移至 `/home/saya-test/ni248-preserved-fixtures-250`，标注 `runtime_cleanup=not proved`、`global_absence=unknown` | `data/native-image-gate-248/receipt.json`、`data/ni248-fixtures-preserved-250/receipt.json` |
| Linux ARM 第 249 轮（保留容量中止） | 78 项包装测试通过（`249-wiring-green-main-20261117.log`）并执行前序双层备份与源码覆盖；`just check` 通过 `browser::*` 全部单元项（含 6 项新／修 worker 复核用例），运行至 `execution::linux::tests::system_alternatives_resolve_read_only_and_stay_subject_to_protected_masks`（同时写入 3 个 113 MB `saya-worker` 临时目录）时因空间降至 `3,186,274,304 B`（低于 `3 GiB` 守卫）而被 `live_limits` 终止并记 `cleanup_unknown`；后置清理确认主进程 78961 进程组已空（`empty_known=True`），原回执保留 `incomplete` | `data/native-image-gate-249/receipt.json`、`data/249-wiring-green-main-20261117.log` |
| Linux ARM 空间回收第 252 轮 | 5 项单测 RED -> GREEN；认证第 249 轮空组证明后安全清理中止残留 `ni249`（拒绝符号链接）与 15 个未引用的 242 前历史 `/home/saya-test/` 测试目录，工作区可用空间由 `3,186,126,848 B` 恢复至 `5,668,450,304 B`（5.28 GiB） | `helpers/reclaim_n5arm22_252.py`、`data/n5arm22-reclaim-252/receipt.json` |
| Linux ARM 第 252 轮完整通过 | 衔接第 248／249／回收第 252 轮已封存输入（4 项包装单测通过），`n5arm22` 三层备份校验通过（631,382,636 B 压缩新增）；`just check`（707.28s、exit 0）72 组结果 1716 passed、0 failed、23 ignored，新 release `/workspace/target/release/saya` 为 66,522,328 B（SHA-256 `166e82bc2375c26cbabe39b93f7ea90a111e1d0101625c115bf37f05a29ecda6`）；`just test-tooling`（2.13s、exit 0）11 suite、168 passed；同程序 `just smoke`（88.35s、exit 0）含审查与 28 组 TUI 旅程通过；3 个运行步骤主进程组均空，post-run 4 项 HTTP 插件只读夹具 `.tmp0XJhRL`／`.tmp9FNOEN`／`.tmpC3qvJ4`／`.tmpNKDY32` 经协作锁与 `renameat2(RENAME_NOREPLACE)` 无损归档至 `/home/saya-test/ni252-preserved-fixtures-252`，`tmp_entries_after=[]`，剩余空间 `4,891,271,168 B` | `data/native-image-gate-252/receipt.json`、`helpers/{run,test}_native_image_gate_252.py` |

上述成果只关闭启动前本地可执行字节与 FD 重核，不提供原子 `execve`／不可变安装快照，也不等于 native kernel 角色绑定或生产捕获完成：`verify_sandbox` 尚未消费 `WorkerConnection::process_info`，定制捕获引擎及 2 处绘制／回读遮蔽缺口仍待修补，`crates/saya-core/tests/browser_screenshot_real.rs` 仍为 `ignored`，`screenshots`／`screenshot_masking` 维持 `false`，新增四平台联合验收尚未完成，未推送、发布或触发远端 CI。

## 固定捕获引擎本地构建

2026-11-16，固定 Chromium `6449d9f9a6a906edff724b7b220199b3fde4615c` 加封存捕获补丁 `a2cba7371192f7c982e5a7decf2ed70e2d8c3db79fbc23b12386121ad86930dc`（23 文件），使用第 180 轮 GN 图、固定 Mac ARM 工具链／SDK 27.0 交叉构建 Linux ARM64 `headless_shell`。`use_remoteexec=false`、`use_siso=false`、Ninja `-j2`；不使用远端构建或新增 CI。以下路径相对 `target/n5/capture-toolchain-20261115/`：

| 轮次 | 实际结果与边界 | 证据 |
| --- | --- | --- |
| 第 183 轮 | 11 个修改 TU 目标预检退出 0；完整构建到 21,600 秒预算中断，最后进度 37,232／37,978。回收出现 EPERM 且后置 index 字节检查失败，完整命令退出与组回收未确认，仍为 incomplete | `evidence/capture-build-183.json`、`data/capture-build-183/` |
| 第 231 轮 | 实际执行剩余 746 步并完成 LINK，270.37 秒、退出 0、stderr 为空；全部所属进程组回收。原 index 字节后置检查仍失败，前后 509,184 个 path／mode／blob 均等于固定 revision，不追改为构建通过 | `evidence/capture-build-231.json`、`data/capture-build-231/full.{stdout,stderr}` |
| 第 232 轮 | 八条查询在独立 inode／nlink1 的 index 副本上执行，原 index／HEAD／refs／config 的字节与元数据前后不变；精确 diff 及 no-ext-diff 对照均未复现副本改写，Trace2 不确定历史写入者 | `evidence/capture-index-232.json`、`data/capture-index-232/` |
| 第 233 轮 | 48 条实际命令、18 次 Root 来源查询及前后守卫通过；原三份 `.git` 禁止写入，Root 查询使用独立 index，Ninja 与嵌套仓库不继承。Ninja 退出 0、no work to do，已有 ELF 身份确认，全部进程组回收 | `evidence/capture-build-233.json`、`data/capture-build-233/` |

第 233 轮只在逻辑成员严格等于固定 revision 后，以第 232 轮已绑定的原 index 字节建立新前置基线；后置不再放宽。该轮没有重写历史报告、修复原 index、修改 Git 配置、清除 object 或制造完成 marker。Root 私有查询的 stat cache 不是生产安装身份，不能用它替代原 Git 元数据与源码检查。48 条命令的原始 stdout／stderr／磁盘记录已重新核对摘要，18 次 Root 查询均保持原状态不变。

本地 ELF 为 `target/n5/capture-build-20261115/src/out/saya_capture_linux_arm64_180/headless_shell`，501,808,968 字节、ELF64 little-endian／aarch64，SHA-256 为 `18dc7e9e243ec5994d6074022452696caadc4e76315b0baac91e0b041d0f8fca`。第 233 轮报告 SHA-256 为 `9f5097366ad11bc8947e60c56b1a279d55700b732d7c2b5e8a4f7a05fecf186f`。增量复验确认既有产物，不声称该轮重编全部对象或已经运行引擎。

`chromium_build_verified=true` 仅属于本轮本地构建范围；`production_capture_verified`、`deployed`、`real_capture_tests_run` 仍为 false。最小运行时包、定制引擎实跑、真实遮蔽与导航／取消屏障仍未验证，不能据此开启截图或认定 native／四平台成功。该阶段未修改 Rust 或重跑完整 Rust 门禁；N5 全范围未完成，未推送、发布或运行新增远端验收。

## 固定程序与本机门禁

宿主为 macOS 27.2 ARM，Rust 1.99.1，部署目标 15.0。固定 release 程序 SHA-256：

```text
86c08738d627d3b54314957675f82f1e23398fab212b4a02fe1410a9f82a0135
```

| 检查 | 实际结果 | 本机证据 |
| --- | --- | --- |
| `RUST_TEST_THREADS=1 just check` | 70 个测试二进制、1650 passed、0 failed、12 ignored；格式、严格 Clippy 和 release 构建通过 | `target/n5/dev082-signoff-check.log` |
| 独立 CLI PTY 回归 | 21 passed；此前并行压力下的终端等待超时未在串行复跑重现 | `target/n5/dev082-signoff-chat.log` |
| Python 工具回归 | 门禁当时 118 项通过，含工作区中的联合验收工具用例；工具后续增量单独核对 | `target/n5/dev082-signoff-tooling.log` |
| smoke／journey／审查回归／TUI | 通过，含 5 项审查回归及 28 组真实 TUI PTY | `target/n5/dev082-signoff-smoke.log` |
| 定向独立复核 | 来源、同页锁、有效媒体投影、图片计数、GC 与后台显示未发现新增实质缺陷 | 对应回归位于 core／CLI／TUI 测试 |

`ignored` 是显式条件入口，不计为已执行的平台测试。工具单测通过也不代表真实 VM 或 UI 旅程通过。

## 真实 Browser 后端

专用 Colima profile `saya-browser`，Linux ARM VM，宿主只使用严格 SSH 通道。显式 manifest 摘要：

```text
e6af5a229682028f9be237a96c1e1f4699292d08b3b5bce88822e56a3d679b46
```

固定版本为 `@playwright/mcp 0.0.85`、Playwright `1.66.0-alpha-2026-11-09`、Chromium `158.0.8127.18`（revision 1254）。这些身份约束不等于验证整个第三方安装树。

`target/n5/dev082-signoff-vm.log` 对同一源码实际重跑三个入口，各 1 passed、0 failed、0 ignored：

| 入口 | 断言与边界 |
| --- | --- |
| `browser_vm_real` | renderer 合法 sandbox argv、`NoNewPrivs=1`、`Seccomp=2`，真实 worker／Chromium 生命周期 |
| `browser_vm_network_real` | 只经 broker 出口、来源、redirect、内网／metadata 拒绝、浏览器实际 netns 内 TCP／UDP 正控制与旁路零命中、STUN 执行标记、取消与清理 |
| `browser_vm_files_real` | 合成凭据、origin／frame／stale／次数限制、实际文件字节与上传原名、后台 artifact 跨 GC、unknown 恢复、截图禁用拒绝及清理 |

运行后另行检查 `/tmp/saya-runs` 与 worker／Chromium／socat 进程，无残留。canary 端口与 broker 观察数量随运行变化，不固定为验收常数。

这些结果只覆盖显式预置 VM，不是 native Browser 或四平台证据。`stderr_initialize_sandbox=false` 是观测值，不作为成功断言；VM renderer 的证据与 native 后端所需的内层 sandbox 证据分别处理。

## 生命周期核验与启动清理增量

2026-11-14，provider 不再永久复用首次 renderer 的 sandbox 证据：成功页面 MCP 调用后重新采样，close／shutdown 作废旧证据。核验开始即隔离生命周期，核验失败、外层 timeout 丢弃 future 或清理失败时禁止继续派发；显式 close／shutdown 确认清理后才允许重建。该证据只表示最近一次完成的采样，不声称持续或全部 renderer 的瞬时保证。

启动进程在首次 catalogue await 前注册 connection／workspace，隔离覆盖初始导航和导航后的 catalogue 刷新，直到 sandbox 核验完成；协议／目录拒绝和外层丢弃不会遗失进程清理句柄。初始导航后目录或 sandbox 核验失败，且清理未确认时返回 `outcome_unknown`，不因 provider session 尚未建立而吞掉未知状态。新增启动回归分别先在连接复用、丢失 shutdown 重试和错误的已知失败断言上失败；证据保留为 `target/n5/fullscope-sandbox-startup-red.log`、`fullscope-sandbox-catalog-timeout-red.log` 和 `fullscope-sandbox-post-navigation-timeout-red.log`。

本轮运行时增量使用新的标准 release，SHA-256 为 `c1e20194d690efae1a9403a1c0892b62d78c146fa38c6da3365902305c2c7a91`，Mach-O 最低部署目标为 15.0；不复用本文历史固定程序的验收结论。

| 检查 | 实际结果 | 证据 |
| --- | --- | --- |
| worker 定向回归 | 48 passed、0 failed | `target/n5/fullscope-lifecycle-worker-green.log` |
| 完整 Rust 门禁 | 71 个测试二进制、1660 passed、0 failed、13 ignored，格式、严格 Clippy、release 构建通过 | `target/n5/fullscope-lifecycle-check.log` |
| Python 工具回归 | 11 个 suite、144 项通过 | `target/n5/fullscope-lifecycle-tooling.log` |
| 真实专用 VM | 生命周期、网络、凭据／文件／后台与遮蔽原语四入口各 1 passed、0 failed、0 ignored；运行前后程序摘要一致 | `target/n5/fullscope-lifecycle-vm.log` |
| CLI／TUI smoke | 离线 PLAN、文本会话、5 项审查回归及 28 组真实 TUI 旅程通过 | `target/n5/fullscope-lifecycle-smoke.log` |

真实生命周期入口实际执行 close／reopen／snapshot；本轮采样 renderer PID 分别为 133754、133857，均为 `NoNewPrivs=1`、`Seccomp=2` 且 argv 未禁用 sandbox，不把 PID 必须改变作为通过条件。前置独立空基线与最终独立清理检查分别保存在 `target/n5/fullscope-sandbox-final-inventory-after.log`、`target/n5/fullscope-lifecycle-inventory-after.log`，远端 Run 根及关联进程均为空。截图入口仍只验证合成页面原语，生产截图 facts 保持 false。此前 1659 项门禁及 `3f0b7bd4…` 程序的复跑日志继续保留，不冒充最终增量证据。

这一增量只在本地验证与提交；按项目负责人最新要求，不推送，新增四平台远端验收等待再次授权。N5 全范围仍未完成。

## Linux VM 宿主通道增量

2026-11-14，VM 宿主通道改为按 Auto 选择 macOS Seatbelt／Linux Bubblewrap；Linux 保留私有 netns、通用 Seccomp、匿名 pair 的内核身份／行为探测与固定 socat 镜像校验。`LoopbackPort` 增加精确 `host`，最多桥接 SSH 与本次 broker 两个固定目标；没有 Host、宿主 netns 或 IPv4 回退。账号解析只使用合成只读 passwd／group／NSS 文件，三项 SSH `read_files` 不扩大为目录授权。实现边界见 [Browser](browser.md#dev-080-vm-后端扩展) 与 [Sandbox](sandbox.md#linux-原生实现)。

离线 Linux 输入归档 `target/n5/linux-host-channel/source-native-green.tar` 的 SHA-256 为 `94fca3dbfafe3bc824b96fce5964254dbcb6d096b3e3eef23aa4945edafbdda4`，显式包含新增 relay 与 Linux 入口，不包含用户既有 `CONTRIBUTING.md` 或实际凭据。Ubuntu 22.04 ARM 用户空间使用 Rust 1.99.1、单并发、关闭 dev debug／incremental，在现有 Colima default 的 `6.8.0-117-generic` 内核中断网构建；这是受控 VM／容器证据，不是 Ubuntu 22.04 原生宿主四平台验收。固定 Linux dev 程序摘要为 `12ac0f03d1c586928c8989911df6d45c5a2c8c9a14f12a059aa21daacabbc770`。

| 检查 | 实际结果 | 本机证据 |
| --- | --- | --- |
| 受管 relay | 双目标转发、IPv6 活动连接关闭、部分启动失败、重复清理后的路径重用均通过 | `target/n5/linux-host-channel/relay-green-reuse.log` |
| Linux 启动协议与 prepare | 两条有界记录、无效／重复目标拒绝、私有通道 prepare 各 1 passed | `target/n5/linux-host-channel/native-green.log` |
| Linux payload 与真实 SSH | 两入口各 1 passed；均覆盖 `127.0.0.2` 与 `::1`。SSH 公钥认证成功、错误 host key 拒绝；payload 只达 SSH／broker，其他 TCP／UDP 无命中，命名 Unix 拒绝，精确文件只读且父目录内容隐藏，512 及以上故意设为可继承的宿主 UDP FD 未进入 payload | `target/n5/linux-host-channel/native-green.log` |
| Linux core 全量 | 同一测试程序在带 init 的夹具中 689 passed、0 failed、1 ignored；显式 prepare 入口另行运行，不把 ignored 算通过 | `target/n5/linux-host-channel/linux-core-init.log` |
| 非 root 原生通道 | 专用 VM 中 uid 501、CapEff=0、NoNewPrivs=1 的宿主进程执行两种地址的真实 payload 回归，1 passed；fixture 已删除 | `target/n5/linux-host-channel/native-nonroot-vm.log` |
| macOS 完整 Rust 门禁 | 1668 passed、0 failed、13 ignored，格式、workspace 全目标严格 Clippy 与 release 构建通过 | `target/n5/linux-host-channel/macos-check.log` |
| Python 工具回归 | 11 个 suite、144 项通过 | `target/n5/linux-host-channel/tooling.log` |

非 root 验证复用已预置专用 VM 的 namespace 前提，仅在私有 mount namespace 的临时 `/usr/local/bin` 视图安装固定 Bubblewrap 0.13.1，未改变 VM 全局工具或 sysctl。程序、测试入口和工具的转移前后摘要一致；宿主进程的 CapEff／NoNewPrivs 观测不是 Chromium sandbox 证据。

失败记录保留：初始 `prepare-red.log` 明确只支持 Seatbelt；`relay-red-final.log` 三项因 Unsupported 失败；`relay-reuse-red.log` 复现清理误删复用路径；`protocol-red-fixedenv.log` 复现未支持双桥记录；`native-red.log` 的 payload 因缺账号解析失败。SSH 夹具最初正控制失败不计产品红灯，改用私有账号目录后正控制通过，`nss-green-ssh-red.log` 随后才在 Linux preflight 平台拒绝处失败。缺 vendor 的 `protocol-red.log` 是准备失败，不是行为验收。

默认 Colima 的非 root 最小 `unshare`／Bubblewrap 正控制同样因 namespace 能力被拒绝，记录于 `rootless-preflight.log`／`native-rootless.log`，未修改其 `apparmor_restrict_unprivileged_userns=1`。无 init 的首轮 Linux core 有三项 orphan 清理失败，记录于 `linux-core.log`；增加实际 init 回收前提后，同一程序全量通过，不修改或放宽清理断言。原有容器外层的测试权限不作为产品隔离能力；产品缺少所需内核／LSM／工具条件时仍失败关闭。

### 初次平台门禁阻塞（历史输入）

系统卷空间耗尽时，本阶段没有标为完成，也没有创建新的阶段 commit 或推送。当时独立测量系统卷仅约 100 MiB 可用，Akari 工作区卷约 34 GiB；本会话可丢弃的临时输出仅约 1.6 MiB，不能据此恢复系统卷空间。没有删除归属不明的缓存、其他会话数据或共享 VM 磁盘，也没有重启／调整共享 Colima VM。

该轮 Linux 全 workspace 的源码输入归档为 `source-platform-final.tar`（SHA-256 `996302652b1dde41e1d81a5202452155a83cbb077a3897db728c819a844890ef`），不包含随后补强的源码。全部失败测试名称、失败 target 与分轮计数另存于 `target/n5/linux-host-channel/verification-summary.json`，保留原始日志，不把无法执行的后续入口算作成功。`linux-workspace-check.log`／`linux-workspace-final.log` 暴露仅供 macOS 使用的测试辅助类型与 AtomicBool 导入未做平台门控，已按实际使用范围修复，未关闭 lint。随后 `linux-platform-final.log` 的格式与全目标严格 Clippy 通过，重新构建的 dev 程序仍为 `12ac0f03…`；但 workspace 测试出现 `Input/output error (os error 5)`、临时 SQLite 数据不可读等失败，完整 Linux 门禁**没有通过**，不能以此前 core／定向入口成功替代。

macOS 中间轮 `macos-check-final.log` 的 `queued_compaction_at_the_last_execution_call_runs_after_completion_without_a_second_user_turn` 曾因 sandbox prepare 的 `Stale NFS file handle (os error 70)` 失败；随后 `macos-platform-final.log` 完整门禁通过，未放宽断言或增加隐式重试。该轮固定 `73df58171170d80ecc73f44d81671f25265c6c74f4a8770af5525d4af0ba2862` 程序通过 smoke／文本 journey、5 项审查回归及 28 组 TUI；真实 VM 四入口各 1 passed，前后独立 Run 根／关联进程均为空，程序摘要一致，日志为 `macos-smoke.log`／`macos-real-vm.log`。截图仍只验证原语，不是生产截图事务。

固定旧测试程序复现了三个真实 VM 入口在缺少显式输入时仍以成功返回；`entrypoints-baseline/missing-input-results.json` 记录 `browser_vm_real`、`browser_vm_network_real`、`browser_vm_files_real` 的 exit=0，而原本已严格检查输入的 `browser_screenshot_real` 为 exit=101。旧程序与失败记录保留；修复及最终源码的重验见下节，不把旧归档或旧测试程序当作后续增量的身份。

### 通道与真实验收断言补强

闲置临时项已按授权迁移到工作区私有归档；后续构建、日志与可控临时输出使用工作区 TMPDIR，不改变系统临时目录链接或全局配置。活动 IPC、工具运行目录及带 Git 元数据的目录保留，不能把迁移描述为删除整个 `/private/tmp`。

受管 relay 新增两项实际 IO 红绿回归：活跃期间替换 socket 路径后，清理按类型及 device／inode 核对，不直接删除替换物；HTTP broker 桥收到首字节后才连接上游，空就绪探测不会先消耗 `max_total_connections=1` 的唯一额度，SSH 服务端先发 banner 仍正常。旧实现确实在两个新断言处失败，修复后 6 项通过，证据为 `relay-review-red.log`／`relay-review-green.log`。

三个旧 VM 入口不再缺前提时成功返回：必须提供并固定 `SAYA_TEST_EXECUTABLE`，必需 VM 字段和端口严格解析，provider 装配失败也失败；不使用默认 release 路径，不按 macOS 之外的平台成功跳过。最新四个测试程序在 `entrypoints-shortpaths/harnesses.json` 记录程序与 Rust 源码 SHA-256，其源码摘要与真实 VM 运行一致；四入口各自的缺输入、缺 VM 设置、程序不存在、程序不可固定共 16 项负控均以 exit=101 在对应前提边界拒绝，实际错误和失败计数均经核对，证据为 `entrypoints-shortpaths/negative/results.json`。较早的 `entrypoints-final/` 冻结轮保留自己的程序身份。截图入口原本已严格拒绝缺前提，本次仅移除 macOS-only 限制。

凭据／文件入口先证明 fill 未自动提交，再显式点击 submit，核对 fixture 实收 `user=alice-secret`（合成测试值），保留 origin／frame／stale／次数拒绝。网络入口以 oneshot 确认慢请求实际到达后才取消，要求派发后的 `outcome_unknown`，不再以未 poll 的入口取消代替。清理扫描使用有界受控 SSH、结构化目录／进程清单和完成标记，不吞权限或 I/O 错误；新增不可列目录的默认回归先在旧扫描上失败，修复后通过，证据为 `cleanup-scan-red.log`／`cleanup-scan-green.log`。

| 最新本机检查 | 实际结果 | `target/n5/linux-host-channel/` 下证据 |
| --- | --- | --- |
| 完整 macOS Rust 门禁 | 1672 passed、0 failed、13 ignored；格式、全目标严格 Clippy、release 构建通过 | `shortpaths-macos-check.log` |
| Python 工具回归 | 11 个 suite、144 项通过 | `shortpaths-tooling.log` |
| CLI／TUI smoke | 离线 PLAN、文本 journey、5 项审查及 28 组 TUI 旅程通过 | `shortpaths-smoke.log` |
| 强化后的真实专用 VM | sandbox 生命周期、网络、凭据／文件／后台、遮蔽原语四入口各 1 passed、0 failed、0 ignored | `shortpaths-macos-real-and-joint.log` |
| 完整 Browser 联合矩阵 | 21 passed、1 disabled、0 failed；Execution 135、Review 8、Compact 1；11 组 PTY 退出无强制 SIGKILL | `shortpaths-browser-joint/run-7wcz2abb/evidence.json` |
| 独立清理与身份 | 前后 Run 根和关联进程均为空；四份测试源码与固定程序前后摘要一致 | `shortpaths-macos-vm-inventory-{before,after}.json`、`shortpaths-macos-vm-identity.json` |
| 前提负控 | 四个新固定测试程序的 16 项均以 exit=101 在预期边界拒绝 | `entrypoints-shortpaths/negative/results.json` |

本轮 release SHA-256 为 `d3c9e061cc5fed584fac42cb9ecd7c3513cf2505c9ab8ff7c6a91287f633583f`，Mach-O 最低部署目标 15.0、构建 SDK 27.0；程序由标准门禁构建，冻结后复跑真实 VM、完整联合矩阵与 smoke，最终再次核对身份。`final-macos-check.log`／`strict-*` 的 1671 项与 `73df5817…` 程序保留为较早输入，不用于替代本轮结果。最新源码归档与逐文件摘要另存于 `source-shortpaths-final.tar`／`source-shortpaths-final.json`，汇总为 `shortpaths-stage-signoff.json`；它们记录当前工作树，不伪称独立构建来源证明，也不复用旧归档摘要。上述完整 Browser 结果仍是 macOS ARM 宿主与显式 Linux ARM 专用 VM，不是 Linux 宿主或四平台 Browser 成功证据。

### Linux 交叉链接与定向实跑

共享 default Colima 的 image／overlayfs 读写仍出现 I/O 错误，`linux-sysroot-probe/copy.log` 记录只读复制 CRT 也因 overlay mount 失败；没有重启、调整或修复共享 VM。健康专用 VM 缺少 `/usr/lib/gcc/aarch64-linux-gnu`，无法直接导出完整 GNU linker 输入，失败记录为 `cross-tools/sysroot-list.log`。

最初 Zig linker 不支持必需 `--fix-cortex-a53-843419` 的失败保留在 `cross-tests.jsonl`／`cross-tests-errors.json`。最终继续用 Zig 编译 C，以 Clang 和 GNU-flavor LLD 链接，保留该 ARM 勘误参数；补入同一 Zig 工具链实际使用的 ARM64 UBSAN 与 compiler runtime，不关闭 C 检查。工作区私有 linker sysroot 来自 Ubuntu Jammy 官方索引，经独立公钥 GPG 验签及 Size／SHA-256 核对，仅提取 28 项所需输入，绝对库别名重定位为私有根内的相对链接；16 项离线工具回归通过，不执行包脚本或安装全局工具。最终 manifest 为 `apt-link-sysroot-20261114-4/manifest.json`（SHA-256 `6c1d9303acb411fde13b5b86c93c315f5820b26c53e57a1af5430b00dac6c846`）。最新 GNU target workspace 全目标严格 Clippy 与完整测试程序 `--no-run` 链接均通过，日志为 `shortpaths-linux-clippy.log`、`cross-tests-shortpaths.jsonl`／`.log`；离线 vendor、Cargo 配置和编译缓存均位于工作区。

第一次链接成功后的真实 prepare 暴露 Unix socket 路径超长，失败轮保留在 `cross-native-vm.log`，当时失败后也确认清理。新增 94 字节私有目录的实际转发回归在旧实现复现 `SUN_LEN` 错误；改为保留完整 UUID 位的 URL-safe 无填充 Base64 目录名及 `r0`／`r1` 短 socket 名，红绿证据为 `relay-longpath-{red,green}.log`。Linux prepare 入口在独立子进程中强制 64 字节 TMPDIR，不改父进程全局环境；broker canary 使用先发 HTTP 请求再读取响应的真实 client-first 协议，SSH 仍允许服务端先发。

| 最新 Linux 实际执行 | 结果 | `target/n5/linux-host-channel/` 下证据 |
| --- | --- | --- |
| relay、启动协议与长路径 prepare | 7＋1＋1 passed，0 failed、0 ignored | `shortpaths-native-vm.log` |
| root payload 与真实 SSH | 两入口各 1 passed，保留 `127.0.0.2`／`::1`、双目标和 TCP／UDP／Unix／高位 FD 负控；公钥认证成功、错误 host key 拒绝 | 同上 |
| uid 501 payload | CapEff=0、NoNewPrivs=1 的宿主进程实际执行，1 passed | 同上 |
| 身份与清理 | 共 12 passed；转移前后程序 SHA-256 一致、root 临时目录基线恢复、关联进程为空、fixture 已删除；独立 Browser inventory 前后为空 | `cross-native-shortpaths/{inputs,remote-identity-before,outcome}.json`、`cleanup.log`，`shortpaths-native-inventory-{before,after}.json` |

Linux dev 程序 SHA-256 为 `8a9f6b0eb735eeb0c4f3d01870aed55d9e53473c16ee6e9d24db8a916dc8a4a6`。实际执行环境为专用 VM 的 Ubuntu 24.04／glibc 2.39／ARM64／`6.8.0-117-generic`，不是 Ubuntu 22.04 原生宿主验收；Bubblewrap 0.13.1 只安装到本次私有 mount namespace 的临时视图，未改变全局工具、sshd 或 sysctl。该历史环境当时没有完整原生 Rust／C 编译工具链及编译期绝对 workspace 路径，上述定向阶段没有完成 Linux workspace 全量运行；后续独立环境的完整结果见下一节。不使用假编译器、隐藏失败入口或放宽清理断言凑成功，宿主进程属性也不作为 Chromium renderer 证据。

本阶段只交付已验证的宿主通道与真实验收加固增量，不提供生产捕获事务、Linux 侧完整 Chromium 联合矩阵或新增四平台证据；不改变截图 facts，N5 全范围仍未完成。所有推送与新增远端验收继续等待项目负责人再次授权。

## Linux ARM 完整原生门禁

2026-11-15，在工作区卷创建独立 Lima `n5arm22` VM：Ubuntu 22.04.5／glibc 2.35／ARM64，4 CPU、4 GiB RAM、24 GiB 虚拟磁盘；无宿主目录挂载、不转发 SSH agent，磁盘实际位于 `target/n5/vmf1/_lima/n5arm22/disk`。原有 Ubuntu 24.04 Browser Colima VM 未重启、调整或改全局工具。新环境使用真实 Rust 1.99.1、GCC 11.4、Just 1.59.0 与 Bubblewrap 0.13.1，Rust／C canary 均实际编译并执行；不是交叉链接或编译器替身。

源码基线为 `557f865a1b884b43d228bf48f591770899b21620`，源码归档 SHA-256 为 `6d42adf72f66aa98e09e7cc80cb71a86362565de354bafd34f20cdd98940b878`。新增的 `execution_backend.rs` 测试夹具补丁 SHA-256 为 `94356a9cb186bc7b2e1a411469422107bc4b774ab43da2567d2a570b042f140f`，仅让 Linux Browser 环境测试在独立进程中先固定真实 CLI，再准备并实际执行 payload；缺少程序或能力时不再成功跳过。生产 Rust 源码、权限和 Seccomp 规则未改。原生构建仅关闭 dev 调试信息与增量缓存、使用单编译／测试并发，release 优化配置未覆盖。完整 Git bundle 补齐归档不包含的真实 Git 元数据；没有伪造 revision、覆盖工作区文件或复制宿主 Git 配置。输入、Git tree 与实际工作树补丁分别记录于下表对应证据，仍不构成独立构建来源证明。

| 本轮检查 | 实际结果 | `target/n5/linux-full-20261115/` 下证据 |
| --- | --- | --- |
| Linux 完整 Rust 门禁 | 1686 passed、0 failed、17 ignored；格式、全目标严格 Clippy、标准 release 构建通过 | `logs/ubuntu22-arm-full-workspace-04.log` |
| Linux Python 工具回归 | 11 个 suite、144 项通过 | 同上 |
| Linux CLI／TUI smoke | PLAN 往返、会话 journey、5 项审查、28 组真实 TUI 旅程通过 | 同上，`evidence/ubuntu22-arm-native-workspace-result-04.json` |
| Linux 原生对抗／性能 | 18 passed、1 not_run（未提供 delegated cgroup）；性能 v2 的 21 项核对通过，每指标 3 次采样 | `evidence/ubuntu22-arm-native-acceptance-06.json`、`logs/ubuntu22-arm-native-acceptance-06.log` |
| Linux 宿主通道定向实跑 | relay 7、启动协议 1、64 字节 TMPDIR 准备 1、root payload 1、root SSH 1、uid 501／CapEff=0／NoNewPrivs=1 payload 1；合计 12 passed、0 failed、0 ignored；私有临时根和关联进程恢复基线 | `evidence/ubuntu22-arm-host-channel-07.json` |
| 同期 macOS 完整门禁 | 1672 passed、0 failed、13 ignored，含格式、严格 Clippy 与 release；工具 144 项及完整 CLI／TUI smoke 通过 | `logs/macos-fixture-{check,tooling,smoke}-03.log` |
| 输入与程序身份 | 基线、测试补丁、环境／配方逐文件 SHA-256，真实 Git tree 与工作树一致；固定程序前后相同 | `evidence/ubuntu22-arm-source-inputs-03.json`、`ubuntu22-arm-git-source-04.json`、`ubuntu22-arm-workspace-runner-result-04.json` |

Linux release SHA-256 为 `9b1634ec7c3cb1b115dee6b9301959bd7e8b86b7574fd7c14ee02d4192660d30`，66,611,320 字节；已复制保全于 `frozen-linux/saya`。macOS release 仍为 `d3c9e061cc5fed584fac42cb9ecd7c3513cf2505c9ab8ff7c6a91287f633583f`，标准重建后与冻结程序相同。两端 `just check` 的 ignored 数量与单独显式入口分别记录，不把忽略、缺前提或 fixture 宿主属性计为 Chromium 成功。密封汇总 `evidence/linux-native-stage-signoff-20261115.json` 核对输入、脚本、日志及报告的逐文件 SHA-256；`stage_verified=true` 仅确认这一阶段，`n5_full_acceptance=false` 保留全部未满足范围。

保留的失败轮次与整改如下：

- 初始 Ubuntu `5.15.0-198-generic` 未满足已确认的内核修复身份门槛，完整 Rust 记录 1540 passed、146 failed、17 ignored；仅在新 VM 安装官方 HWE 后实际启动 `6.8.0-149-generic`，没有放宽 gate 或伪造 uname。第二轮仍有 4 个失败 target、47 项失败，原始日志均保留。
- `umask=0002` 使普通测试夹具产生组可写目录。内存、日志与 CLI 的三组同程序对照均为 0002 失败、0022 通过；只固定验收 Shell 的 umask，不修改生产权限拒绝。对照为 `evidence/ubuntu22-arm-umask-red-green.json`。
- 测试 harness 被误当成 payload launcher 时，实际返回 `Unrecognized option: 'env-fd'`。真实 CLI 提前 pin 与独立进程修正后，原失败测试和完整 workspace 均通过；红测为 `logs/ubuntu22-arm-browser-launcher-red.log`。
- 第三轮 Rust 与 release 已通过，但工具回归在编译器调用之前因归档缺少 Git 元数据失败，未执行 smoke；第四轮补入经过核验的真实 bundle 后整链通过。没有修改编译失败证据测试或伪造 Git 输出。
- 宿主通道编排的第 04 轮缺少显式 `--ignored` 被实际计数检查拒绝，第 05 轮错误地给 root 使用 uid501／0700 的 socket 祖先，第 06 轮切换用户时被 sudo 清除了测试程序变量。分别修正显式调用、按 UID 分离临时根及精确重建测试环境；未放宽命名空间的目录访问或继承任意宿主环境。第 07 轮全部实跑并核对清理，失败报告未改写为成功。

本轮完成 Linux ARM 的完整原生 workspace 门禁和这一阶段回归，不代表 native Chromium、生产截图、macOS 15 运行环境或四平台联合完成。截图 facts 保持 false，新增远端验收与推送仍未进行。

## DEV-083 联合矩阵

[真实入口工具](../scripts/n5/acceptance.py)只使用显式程序、本地模型与本地网页，不 provision、不下载、不访问付费服务。缺少 `SAYA_TEST_VM_*` 前提必须失败并留证，不能把未运行算为通过。本轮新增 Browser 场景使用受控 Chat Completions fixture；三协议正常入口回归单列，不扩称为三协议 Browser 联合实跑。

| 矩阵 | 当前状态 |
| --- | --- |
| CLI／TUI：PLAN × 三权限、auto 核查、Manual 批准／拒绝、bypass | 正常入口通过；11 组真实 PTY、48 帧逐调用审批，填充正文不出现在审批中，终端完整属性与 fd flags 恢复 |
| Browser：长快照、OutputRef 检索、文件、后台慢页、unknown 与恢复 | 正常 CLI／TUI 通过；全量分页到 EOF、下载实际字节／同 Session 上传、后台及前台取消、unknown 不重放与新快照恢复 |
| Browser × 插件 × 显式 Compact × Session 恢复 | 实际安装／激活／读取插件资源，Compact 仅摘要调用；同 Session 恢复后检索原 OutputRef，原始历史与工具配对不改写 |
| 三协议 N3 CLI／TUI 回归 | 修复夹具后重跑通过 34 条 CLI、15 条真实 TUI 旅程；CLI 实际请求 105 个工具、观察 102 个结果，Manual 等待单列 |
| 原生隔离与性能 | 固定程序记录 18 passed、1 not_run（未提供 Linux delegated cgroup）；性能 v2 全部 21 项核对通过，每指标 3 次采样 |
| Python 工具回归 | `just test-tooling` 144 项通过，含 36 项 N5 验收工具回归；日志 `target/n5/dev083-tooling-final.log` |
| 固定程序完整 smoke | 离线 smoke、文本 journey、5 项审查回归和 28 组 TUI 再次通过；日志 `target/n5/dev083-smoke-lifecycle.log` |
| 生产截图 | disabled；不计为截图验收通过 |

最终 Browser 报告为 `target/n5/dev083-browser-final/run-jfraywh6/evidence.json`：21 passed、1 disabled、0 failed，实际模型 fixture 请求为 Execution 135、Review 8、Compact 1，未使用付费模型。两条长快照链路各读取 6 页、45,261 字节，完整视图摘要与宿主一致，同时单独记录正文摘要及 200 个有序源标记。前后远端 Run 根／关联进程列表均为空，11 组 PTY 清理均正常退出且未强制 SIGKILL；SQLite integrity 为 `ok`，前台取消意图持久化为 `unknown`／`outcome_unknown`。另行严格 SSH 只读核对仍为空，日志 `target/n5/dev083-final-inventory.log`。

冻结记录为 `target/n5/dev083-browser-view-identity.json`，8 个验收脚本的前后 SHA-256 一致，程序前后均为上述固定值。验收报告的 workspace manifest 不是构建证明；程序由标准 `just release` 生成，日志 `target/n5/dev083-build.log`，对应 Rust 源码 `1443291511a294a141908633296e0d304f68f8bb`、Mach-O 最低部署目标 15.0。本阶段未修改 Rust 源码，曾通过标准构建入口重建并核对固定哈希；最终冻结矩阵期间未重建或替换程序。

修复夹具后的三协议复跑报告为 `target/n5/dev083-n3-cli-lifecycle.json`、`dev083-n3-tui-lifecycle.json`；此前的 `dev083-n3-cli.json`、`dev083-n3-tui.json` 作为早期复跑保留。原生与性能报告为 `target/n5/dev083-native-final.json`、`dev083-performance-final.json`，均保持固定程序身份；不以历史 N2／N3 四平台结果替代当前 N5 的平台证据。

逐项汇总与上述证据文件的 SHA-256 保存在 `target/n5/dev083-stage-signoff.json`；`stage_verified=true` 仅代表这一可验证的本机阶段，`n5_full_acceptance=false` 明确保留未满足范围。

首轮原生证据为 `target/n5/dev083-native-evidence.json` 与 `dev083-performance-verification.json`，明确记录当时工作区源码 manifest 而非伪造固定构建来源。性能报告覆盖实际采样、分位数、HTTP／CONNECT 与 CLI 进程资源；不建立性能 SLO，也不测量并发 Run 的同时总内存。

固定提交的采样摘要如下，单位为毫秒，每指标 3 次，分位数用 nearest-rank；此时 P95 即样本最大值，不代表统计稳定的尾延迟。采样未隔离同机其它验收负载，不能据小差异推断性能提升；冷启动指新 CLI 进程，保留 OS 页缓存。warm command 是同一环境内从模型响应发出到下一工具结果请求到达的区间，其它表中主指标包含相应正常 CLI 入口成本。

| 指标 | Host P50／P95 | Seatbelt P50／P95 |
| --- | --- | --- |
| 冷启动 | 40.38／49.78 | 89.19／89.19 |
| 同 Run 暖命令 | 82.16／82.65 | 88.01／90.23 |
| 短命令 | 134.19／137.28 | 622.89／676.70 |
| 文件写入 | 541.59／577.37 | 600.12／698.63 |
| 文本检索 | 539.99／549.41 | 520.34／552.30 |
| HTTP RTT | 0.887／0.911（直连） | 1.692／1.697（broker） |
| CONNECT RTT | 同上直连基线 | 1.507／1.530（broker） |

### 失败轮次与判据整改

- 普通 `cargo build` 未继承项目部署目标，产物哈希变化后被候选身份校验拒绝，实际旅程数为 0。拒绝证据保留为 `target/n5/dev083-n3-cli-identity-rejected.json`；改用标准构建入口后恢复原哈希与 15.0 部署目标，没有修改预期哈希放行。
- Browser 首轮报告 `target/n5/dev083-browser/run-pgv61cvb/evidence.json` 为 6 passed、16 failed、1 disabled。800 段页面触及 512 元素上限，宿主正确返回 `truncated`／`limit_reached`／`retained_ranges_only`，但脚本错误要求完整；未清除的历史 fixture 错误又污染后续独立场景。这些失败不等于 16 个独立运行时缺陷。
- 长后台 job ID 在真实面板中换行，脚本不能要求单行完整 ID；面板打开时必须先关闭，再通过命令执行取消。前台工具可见性应等待真实 UI 帧，不以 HTTP 命中瞬间的旧画面判断。
- 失败夹具强制结束 TUI 后留下两个无进程的 Run 目录，已仅对本次已知目录调用生产清理路径并保留 `target/n5/dev083-fixture-failure-cleanup-final.log`。强制退出不作为正常 Run 清理通过证据，修正后的矩阵需独立证明正常取消与退出。
- canonical CLI 不复用 TUI 的 raw-mode 预热，完整 `tcgetattr`／`F_GETFL` 基线在候选进程启动前捕获，退出时不掩码 PENDIN 或重采样。真实故障程序在启动时关闭 ECHO，负向回归先失败、修复后通过，防止把错误的启动状态当作基线。
- 冻结轮 `target/n5/dev083-browser-final/run-sgqb_t36/evidence.json` 为 19 passed、3 failed、1 disabled，脚本和程序前后摘要一致，远端前后均为空。新增长快照全量分页校验错误地把密封记录摘要当作正文摘要，导致两条旅程与全局夹具断言失败；实际持久化的 45,261 字节、200 个有序标记均完整，按记录格式重建的视图摘要与宿主一致。验收工具现分别核对完整视图摘要并记录正文摘要，不修改运行时或放宽完整性要求。
- 持续 PTY 读取失败与父进程退出后残留进程组均以真实受控子进程复现并补修；清理只作用于夹具自己创建的进程。相关失败仍判为失败，不能以强制回收冒充正常退出。独立回收探针见 `target/n5/dev083-process-cleanup-probe.log`。

上述诊断轮保持失败记录；最终修正轮为 `run-jfraywh6`，只有冻结后的完整实跑结果计入本机联合阶段通过。

## 截图与媒体边界

### 真实遮蔽原语调查（尚未生产接入）

新增[专用 VM 像素回归](../crates/saya-core/tests/browser_screenshot_real.rs)，仅使用合成字段与 `about:blank`。实际固定 Chromium 返回 `HeadlessChrome/158.0.8127.18`、revision `6449d9f9a6a906edff724b7b220199b3fde4615c`。在已确认的双层 sandbox 内，暂停 renderer 后通过原生 DOM/CSS 遍历并隐藏普通、密码和 closed-shadow 输入；宿主独立解码实际 PNG，逐点核对三处敏感区域为白色、非敏感绿色区域仍保留，同时核对暂停期间计时不变、原始 style 精确恢复后计时继续。PNG 解码依赖仅用于测试。

显式入口实际为 1 passed、0 failed、0 ignored，日志 `target/n5/fullscope-primitives-real-final.log`；此前原语轮次及独立空 inventory 分别保留在 `target/n5/fullscope-cdp-real.log`、`target/n5/fullscope-primitives-inventory.log`。该入口在默认 Rust 门禁中仍为 ignored，缺少显式 VM 输入或平台不符不得返回通过。单 renderer 的特征回归不证明流式文档、导航提交、新 target、OOPIF 或取消恢复的生产事务屏障，不能据此开启截图 facts 或计为 N5 截图验收完成。

这一测试增量的最终 `RUST_TEST_THREADS=1 just check` 通过 71 个测试二进制、1650 passed、0 failed、13 ignored，含格式、严格 Clippy 与 release 构建，日志 `target/n5/fullscope-primitives-check-final.log`；`just test-tooling` 144 项通过，日志 `target/n5/fullscope-primitives-tooling.log`。新增的 ignored 入口已按上一段单独显式执行，不把其余 ignored 算作实跑。初次门禁中的常量平台断言被严格 Clippy 拒绝，已改用仍会在错误平台失败的 OS 断言；初始日志保留在 `target/n5/fullscope-primitives-check.log`。运行时源码未改，release 程序仍为本文固定 SHA-256。

### 生产边界

- 生产 worker 缺少截图前真实遮蔽契约，`browser_screenshot` 不注册；直接调用为 `browser_capability_disabled`，不保存未遮蔽图片。
- 受控 provider 已验证截图宿主成功路径、lease／GC／恢复和三协议原生图片块。它不是生产 worker 的像素级遮蔽证据。
- 开启 profile 图片能力时，只从有效请求历史选择宿主 Image 引用，最多 8 个，raw 与 base64 聚合各不超过 64 MiB；File 永不注入，Compact／Review 不扩展媒体。
- 截图 URL 使用 `last_confirmed` 语义。公开 artifact 句柄不授予跨 Session 权限；页面伪造引用不产生可信 GC pin 或宿主授权。

## 定制捕获引擎的构建前提

[D-068](decisions.md) 已确认允许扩展托管捕获引擎与固定身份契约，必要时采用定制 Chromium；它不是完成证明。[引擎实施边界](browser.md#托管捕获引擎扩展d-068)保留预捕获遮蔽、事务屏障与双层 sandbox。固定源码的 Linux 构建文档要求 x86-64 构建机与至少 100 GB 空闲空间。2026-11-14 的约 31 GiB 余量保留为历史测量，不能当成后续清理后的现值；Linux ARM VM 的运行时验收也不证明标准 Linux 构建工具链支持 ARM 宿主。

### 固定宿主工具链与跨目标执行

2026-11-15，按唯一 `(device, inode)` 核对四个 Cargo profile 的可再生中间产物，并在 `target/` 内定位全部相关硬链接。实际回收 169,521 个 inode、525,390 个路径，候选唯一分配量为 64,388,321,280 字节；回收期间工作区可用量由 41,713,999,872 升至 106,195,374,080 字节。后者是当时的可用空间实测变化，包含并行 I/O 与文件系统影响，不把 `st_blocks` 计量直接当作空间承诺。四个 `.cargo-lock` 均以非阻塞独占锁持有，重新检查未见 Cargo／rustc 或打开的候选文件；分析器仍运行，不宣称取得所有用户进程的未来使用保证。841 个未知或保护 inode 未提升为可回收项；全部程序、动态库、源码、日志、证据和用户 `CONTRIBUTING.md` 保留。149 个受保护程序路径（81 个唯一 inode）的前后摘要一致；已重建的当前 `execution_backend` 与历史冻结副本分别核对，不要求二者冒充同一程序。证据为 `target/n5/engine-cache-inventory/reclaim-applied-01/result.json`。

从固定 Chromium `6449d9f9a6a906edff724b7b220199b3fde4615c` 的 DEPS 绑定官方对象、generation、精确大小与完整 SHA-256，实际取得 Mac ARM64 Clang／LLD 24、同修订的 Rust 工具链、Linux ARM64 sysroot 和 Linux 编译运行库，共 324,108,436 字节。大请求超时与不完整响应保留；续传使用有界范围、核对 `Content-Range`，拼合后仍要求完整固定摘要一致。下载与解包各使用工作区内独立数据目录，未执行归档内安装脚本；工具、sysroot 的文件／链接清单与摘要在 `target/n5/capture-toolchain-20261115/evidence/`。POSIX sysroot 中 systemd 单元名的字面反斜线不作为路径分隔符，仍保留绝对路径、`..`、特殊文件、越界链接、权限与资源上限检查。

固定宿主 Clang／LLD 24 与 Rust 1.101.0-nightly 已实际启动；项目自身仍使用 Rust 1.99.1。独立 C++／Rust ABI 探针经 `aarch64-linux-gnu` 目标与固定 sysroot、运行库链接，保留 `--fix-cortex-a53-843419`；ELF SHA-256 为 `0008bb6a83b52960ac93dfcb97903d889844a54f87fb0b68b0db782ad2c091d9`。同一字节在独立 Ubuntu 22.04 ARM VM 的 uid501、`6.8.0-149-generic` 内核上实跑，输出 `SAYA_CAPTURE_TOOLCHAIN_ARM64_OK`，远端临时程序与目录已清理。Rust 部分明确使用 `no_core`，**不证明目标 std 或 Chromium 链接成功**；固定 GN 配置的目标 std 从源码构建，不能把宿主包缺少预编译 Linux std 误判为默认阻断。完整报告为 `target/n5/capture-toolchain-20261115/evidence/standalone-canary-06.json`。

构建写入约束的真实正负控制已通过：允许两个任务目录内写入，拒绝修改目录外的、由当前用户拥有且模式为 0600 的可写对照文件。后续构建使用任务私有 HOME／TMPDIR／缓存与独立写入限制，不仅依赖缓存环境变量；该对照不是 CIPD、vpython 或完整构建已经通过的证明。固定 depot_tools `b86fc981b72ab966f8260f73b4eca600b3b12677` 已取得；完整源码展开、GN 生成、目标 std 与 headless_shell 的端到端构建仍不属于上述探针成功范围。

尚未运行或部署定制 Chromium，未实现生产捕获事务或身份 v2。本阶段已重新核对四份完整归档、25,515 个文件／链接条目与实际 ELF，密封汇总为 `target/n5/capture-toolchain-20261115/evidence/toolchain-stage-signoff.json`；汇总明确 `target_stdlib_verified`、`gn_generation_verified`、`chromium_build_verified`、`production_capture_verified` 与 `n5_full_acceptance` 均为 false。生产截图继续禁用；四平台远端验收按项目负责人最新要求等待再次授权，不以工具链探针、TCG 可行性调查或交叉产物代替实际平台成功证据。

### 固定 bootstrap 与 GN

2026-11-15，固定 depot_tools `b86fc981b72ab966f8260f73b4eca600b3b12677` 的原生 CIPD 客户端 2.9.2（SHA-256 `4ac2fa37f23486d249a2d71ff5084b6207c00cfa55928dad577fe99f0376ab71`）完成 15 个 Mac ARM bootstrap 包的正式部署。包共 268,172,212 字节，逐包核对 instance、完整摘要、ZIP 路径与展开大小，并由原客户端 `pkg-inspect`／`pkg-deploy` 验证；完整离线 `ensure` 通过，固定入口的 Python 为 3.11.8。实际执行了 gclient 入口；其 `--version` 输出为帮助，不把它记作数字版本，身份仍由固定提交决定。报告为 `target/n5/capture-toolchain-20261115/evidence/{cipd-packages-17,depot-deploy-18}.json`。

同一 DEPS 的 `gn/gn/mac-arm64` 已通过原客户端正式获取、检查与部署；instance 为 `phtG5eQKjHsVLHUJ3vtBlBU6H_pRwldzL4kqKWh1LOkC`，1,087,354 字节的包 SHA-256 为 `a61b46e5e40a8c7b152c7509defb4194153a1ffa51c257732f892a2968752ce9`，实际 `gn --version` 为 `2603 (938087829fcb)`。已用该程序格式化捕获草案的 GN 文件；**未执行 GN 生成或定制 headless_shell 编译**。原始报告为 `evidence/gn-input-23.json`，重新核对全部 bootstrap 包、客户端、depot_tools 提交及实际 GN 版本的汇总为 `evidence/bootstrap-stage-signoff-28.json`，两者均位于上述工具链目录。

完整 Chromium 工作树仍在补齐；逐批 Git 传输超时和官方固定快照在 1200 秒仅取得 23,363,584 字节的失败均保留。快照的 `download_complete` 为 false，未调用导入器，不将部分归档当成完整源码。原生捕获的 C++／Mojo／PDL 草案仅保留于工作区 `target/`，尚未编译、部署或提交为生产实现；既有引擎的六项显式原生用例均在缺少私有命令处失败，不能证明草案正确。生产截图 facts、身份 v1 和已发布能力均未改变。此阶段的成功仅限固定 bootstrap 与 GN；N5 全范围仍未完成，未推送或新增远端验收。

### 捕获草案的生成与固定补丁

2026-11-15，在完整引擎构建前先对固定源码的生成链做独立检查。固定 Python 3.11.8 与上游 Inspector Protocol 生成器分别处理原版、捕获版 `HeadlessExperimental` PDL 和各自配置；两轮均实际退出 0，捕获版生成了两个异步回调，原有 BeginFrame 接口保留。该检查只生成此 domain，不等于全部 CDP domains 或 GN 生成通过；输入、配置、生成物摘要见 `target/n5/capture-toolchain-20261115/evidence/{protocol-generator-inputs-27,protocol-generation-31}.json`。

私有 Mojo 使用同一固定源码的真实 parser、C++ 模板预编译和 typemap 生成器；保留 `gfx::Size`／`gfx::PointF` 原生映射，variant、shared、message IDs 三组生成均实际退出 0。284 份输入逐一核对 Git blob 与 SHA-256；最初一个模板的 EOF 传输失败保留，后续补齐后才执行。报告为 `evidence/{mojo-generator-inputs-36,mojo-generation-37}.json`，位于上述工具链目录。该独立生成未覆盖完整 GN feature／import 配置，也不是 C++ 编译或链接成功证据。

草案已封存为 `target/n5/capture-patch-20261115/data/patch-identity-44/capture-draft.patch`，62,982 字节，SHA-256 `a2cba7371192f7c982e5a7decf2ed70e2d8c3db79fbc23b12386121ad86930dc`。修改前输入按固定 revision、Git blob 和 SHA-256 核对，补丁通过独立 index 上的 `git apply --cached --check`；未改源码工作树、HEAD 或 refs。逐文件身份见 `target/n5/capture-patch-20261115/evidence/patch-identity-44.json`。草案包含预绘制敏感子树抑制、透明清零、滚动几何、文档／激活关联、错误分类及绝对期限复核；**仍未编译、部署或作为生产实现提交**，静态检查不证明这些行为正确。

新增的八项真实原生契约覆盖字段与 closed-shadow、同文档陈旧 context、Canvas 新绘制、内联编辑区、滚动视口、透明像素、不完整绘制与原生期限。前六项在 `target/n5/cap1/logs/native-contract-red-16.log`、加强后的整片内联区域在 `native-inline-region-red-19.log`、后两项在 `native-error-contract-red-32.log` 均实际止于旧引擎缺少私有命令；没有原生 GREEN。最新 `capture-contract-workspace-33.log` 的完整 `just check` 为 1672 passed、0 failed、21 ignored，含格式、严格 Clippy 和 release；忽略项不算原生验收。此前 `capture-contract-workspace-20.log` 的终端慢输出失败与后续锁中毒保留，原断言复验 `terminal-recheck-22.log` 的 9 项及后续完整门禁通过，未放宽测试。生产截图与身份 v2 仍未交付，N5 全范围仍未完成，未推送。

### 固定完整源码与专用捕获环境

2026-11-15，固定 Chromium revision 的 474,351 个唯一 blob 已全部补齐。逐批 Git 续接与原始文件补传失败均保留；最后 13 个对象改用有界分段，复用的旧部分字节不单独算通过，只有完整 Git blob 摘要匹配后才写入对象库。最终一个分段的 EOF 在独立目录重取后才拼合；没有导入不完整官方快照、清理 pack 或改写 refs。`git fsck --full --strict --no-dangling` 实际退出 0，安全展开后 HEAD 仍为 `6449d9f9a6a906edff724b7b220199b3fde4615c`，当时工作树 clean；Git tree 清单与 DEPS 的 SHA-256 分别为 `90d47707b34d633d8f74852bbfb79ae54a1c80da4a73128a2e89c42a2ee5d818`、`3cc6c8cf39333b06915b52ee89352fa0c74607929d74b8e9f41b868c6595be81`。报告为 `target/n5/capture-toolchain-20261115/evidence/source-checkout-50.json`；分段与重取记录为 `source-ranges-47.json`、`source-range-retry-49.json`。完整源码不等于 DEPS、hooks、GN 或引擎构建通过。后续首次 DEPS 同步在 vpython 派生包构建器试图写入 `/tmp` 时被工作区写入约束拒绝，报告为同目录 `deps-sync-51.json`；没有放行目录外写入、绕过 vpython 或将其记作同步成功。

本任务独有 `n5cap` VM 已恢复私有 usernet，正常 cloud-init 完成；退出码 2 来自上游配置弃用告警，实际 `errors=[]`，当前 instance-id 与 boot-done 匹配。停机磁盘副本和失败日志保留，未格式化或调整原有共享／Browser VM。实际 Lima 资源为 2 CPU／2 GiB、Ubuntu 24.04 ARM64／`6.8.0-117-generic`，无宿主挂载、无 SSH agent 转发。首次 rsync 安装引起的启动等待及随后 Chromium 大依赖下载的超时保留；后者从该 VM 的 APT 元数据绑定剩余 23 包的完整 SHA-256，经 HTTPS 分段预取、宿主与 VM 双重核对后交回正常 `apt-get --no-download` 安装。不使用 `--print-uris` 的 MD5 作为完整性门槛，不换镜像源或绕过签名，不改旧 VM 的工具或 sysctl。

固定 stock runtime 已通过七个命名文件及 manifest 校验；仍是身份 v1，不声称封闭整个安装树。`target/n5/cap1/logs/runtime-baseline-48.log` 的真实原语基线 1 passed，固定 Chromium 版本／revision 匹配，renderer 的 NoNewPrivs=1、Seccomp=2 与合法 sandbox argv 断言通过；closed-shadow／密码合成字段未绘入图片、绿色正控制保留，页面样式及暂停后计时恢复。独立前后 inventory 的 Run 条目与关联进程均为空。环境报告为 `target/n5/cap1/evidence/runtime-provision-46.json`，包预载记录为 `package-preload-45.json`，inventory 为 `data/runtime-baseline-48/{before,after}.json`。该成功仅证明新专用环境可执行已有原语，**不是原生生产捕获、native Browser 或四平台成功**；N5 全范围仍未完成，未推送。

### 固定构建工具的工作区临时目录

2026-11-15，确认首次 DEPS 同步的直接失败来自固定 depot_tools `Gsutil.get_sub_env`：它在非 Windows 上无条件把三个临时变量改为 `/tmp`。新增真实受控子进程回归先观察三个值均被覆盖的 RED，再将实现改为仅为缺失变量补默认；未改 BOTO、Windows、真实 vpython、固定 spec 或 bootstrap。新三项回归通过；第 53 轮真实入口保留 `/tmp` 拒绝，第 54 轮虽打印版本且退出 0，stderr 仍含 Unix socket 异常及探针自身空 AWS 配置的告警，旧报告的宽松标记**不作为干净成功证据**。后将探针收紧为 stderr 必须为空、去掉自身注入的空配置，并在私有无凭据 HOME 与短临时根下实际通过第 61 轮，未读取真实凭据、过滤告警或绕过 bootstrap。报告为 `target/n5/capture-toolchain-20261115/evidence/depot-runtime-{53,54,61}.json`，单元回归日志为同根 `logs/depot-temp-{red-52,green-54}.log`。

[构建工具补丁](../scripts/n5/patches/depot-tools-workspace-temp.patch)基于上游 `b86fc981b72ab966f8260f73b4eca600b3b12677`，3,612 字节，SHA-256 `5d3983146fc8e4064b26273c66462ef9e1ad823430d7697567ce244ecbcbdb05`。两份修改文件的原始／新摘要及独立 index 应用检查见 `evidence/depot-patch-64.json`，其中运行证据绑定空 stderr 的第 61 轮；此前第 58 轮的静态补丁身份仍有效，但不能用其第 54 轮宽松运行标记代替干净成功。HEAD 与 refs 未改。后续工具身份必须同时绑定上游提交和此补丁，不再将该本地副本描述为 pristine，也不把工作区补丁当成上游已接受的变更。

上游完整文件首次运行 30 passed、1 failed：`GstoolsUnitTests.test_gsutil` 的真实 `SyncManager` 因 macOS `AF_UNIX path too long` 向 stderr 写入错误，未放宽空 stderr 断言。随后只把私有临时根从构建目录内 `tmp/` 缩短为 `t/`，完整文件 31 项通过；原目录未删除、写入约束未扩大，默认无显式变量时的 `/tmp` 语义未改。日志为 `logs/depot-download-suite-{55,59}.log`，完整失败原因见 `depot-download-failure-57.log`。上游 `test_get_md5_cached_write` 原有未关闭文件的 `ResourceWarning` 在成功轮仍存在，不能称为零告警。封存初次因 detached checkout 无 refs 导致 `show-ref` 退出 1 的失败保留于 `depot-patch-56.log`，改用可表达空 refs 的枚举后才通过；不是跳过 refs 核对。

项目 `just test-tooling` 的 11 个 suite、145 项全部通过，日志为 `logs/project-tooling-55.log`；此前构建专用 PATH 不含 `just` 的第 54 轮未启动，后明确使用已存在的任务执行器重跑。上述相对证据路径均位于 `target/n5/capture-toolchain-20261115/`。该阶段仅证明临时目录修复、真实工具入口和相关回归，**不证明 DEPS／hooks／GN、引擎编译或生产捕获成功**；八项原生捕获仍无 GREEN，截图关闭与身份 v1 保持不变，未推送或新增远端验收。

### 固定 SDK 查询与原生依赖缓存

2026-11-15，`xcrun` 的 SDK build 查询虽曾退出 0，仍因系统缓存写入被拒绝而向 stderr 报错；第 66、67 轮不算干净成功，`--no-cache` 也不能禁止缓存写入。核对实际工具实现后，以绝对 `xcrun_db` 指向工作区缓存，第 68 轮四项真实查询均退出 0 且 stderr 为空；第 71 轮未修改的正式 `build/config/apple/sdk_info.py` 也通过相同约束，生成链接仅位于任务输出内。SDK 为 27.0／26A425，52,589 条文件／目录／链接、931,393,813 文件字节的 canonical tree SHA-256 为 `17e860dfd2454374e088331f45693e0e8b7aceef0af1530c96b1ff0774cde5c5`；不跟随树内链接，不修改 Xcode。查询脚本和实际查询库身份分别记录于 `evidence/sdk-query-71.json`，完整 SDK 身份见 `evidence/sdk-identity-68.json`；这不是全部 Xcode 工具已封闭或 GN 生成成功的证明。

第 60 轮真实 `gclient sync --nohooks --noprehooks --no-history --shallow --jobs=2` 在 1800 秒上限被终止，报告 `evidence/deps-sync-60.json` 的 `dependencies_synced=false`。日志显示固定 trace／sysroot 的传输停滞，没有新的临时目录拒绝。第 69 轮默认端点与第 72 轮显式 TLS 校验的 XML 端点对照中，真实 stat 约 3–4.5 秒成功，但两次 cp 均在 180 秒超时；同一对象的 curl 全量请求在 120 秒只取得 3,031,031 字节，完整摘要不匹配。第 70 轮已主动停止，不作为成功证据；也不能据此声称实际发生了未校验证书的传输，原程序默认启用校验。原始报告 `evidence/gcs-transport-{69,72}.json`、部分下载和失败日志全部保留。

[固定 GCS 缓存补丁](../scripts/n5/patches/depot-tools-gcs-cache.patch)基于同一上游 `b86fc981b72ab966f8260f73b4eca600b3b12677`，11,279 字节，SHA-256 `11d68313ff90b2e9f40f07330ef85cebd3e6a438788876c568c36ec7e98cb818`。仅在 POSIX 构建中显式设置绝对 `GCLIENT_GCS_DOWNLOAD_CACHE` 时启用，以固定小写 SHA-256 作为文件名；拒绝非普通文件、符号链接、错误大小与摘要，FIFO 不阻塞，坏输入不静默回退。缓存未配置或缺失时保留原下载路径；缓存命中后仍由原生方法验证 SHA／大小、验证及解包归档、通过真实 gsutil 查询 executable metadata，最后才写原生完成标记。没有设置 `GCLIENT_TEST`、复制提取树或手写完成标记。

定向测试包含有效缓存、缺失／未配置、等长坏摘要、独立大小错误、非法 key、相对目录、symlink、带 5 秒子进程期限的 FIFO，以及远端 stat 失败时不写完成标记。第 73、75、76 轮 RED 分别保留，最终 10 项与完整 gclient 98 项在 `logs/{gcs-cache-green,gclient-suite}-79.log` 通过；完整下载工具 31 项见 `logs/depot-download-suite-80.log`，原有未关闭文件 ResourceWarning 保留。项目 `just test-tooling` 的 11 个 suite、145 项见 `logs/project-tooling-86.log`；期间暴露的显式临时目录问题与 PTY 测试策略分别见[进度记录](progress.md#原生验收的显式临时目录)，固定构建写入策略未扩大。

第 74 轮预取完成 4 份工具链／sysroot 归档和 21 个固定 trace，共 25 个真实缓存输入，逐一核对完整大小和 SHA，未写原生标记。范围下载逐段验证 Content-Range，拼合后仍要求完整摘要匹配，记录为 `evidence/gcs-prefetch-74.json`。第 79 轮真实单 sysroot 导入虽成功，但未独立记录缓存命中和内部 stat stderr，不用外围空 stderr 推定内部无告警。第 85 轮保留真实执行路径，以解释器文件打开审计记录实际缓存读取，并以不替换返回值的透传观察器保存真实 gsutil 操作；只观察到一次 stat、没有 cp，stat 退出 0 且原始 stderr 为空。18,015,920 字节 sysroot 的原生摘要、解包、stdio.h 与原生标记核对通过，总计约 7.5 秒；报告和原始 metadata 分别为 `evidence/gcs-import-85.json`、`data/gcs-import-85/gsutil-operations.json`。这仅证明一个真实对象导入，**不证明全部 DEPS 同步**。

临时目录与缓存补丁对应的四文件原始 git diff 为 14,894 字节，SHA-256 `5c0821c0ae9a89fc67d3878c239cfb953c359944d186b405bdd0bb5a81ab7725`。分发缓存补丁省略空白上下文行的单字符前缀，以通过仓库尾随空白检查；第 89 轮在默认 git apply 下顺序应用两组补丁，逐文件结果仍与实际工具副本一致。四份原始／修改文件的摘要、两组独立 index 顺序应用后的逐文件比对、HEAD／refs 不变、25 份缓存复核及真实入口／测试日志摘要，统一见 `evidence/depot-patch-89.json`。初次封存第 81 轮因错误假定缓冲日志必须以 OK 结尾而拒绝，保留失败日志；修正为逐 suite 核对真实摘要且拒绝 FAILED／ERROR／FAIL 后才接受，不把实际失败的第 80、84 轮项目门禁算作通过。后续同步必须绑定固定上游和完整组合补丁，不得继续使用仅含两文件的旧身份；源码内嵌的同修订 depot_tools 仍需在正确同步／hooks 顺序中核对适用补丁。上述相对证据均位于 `target/n5/capture-toolchain-20261115/`。此阶段未完成全量 DEPS、pre-hooks／hooks、GN 或 Chromium 构建，生产截图仍关闭、身份仍为 v1，N5 全范围未完成，未推送或新增远端验收。

### 根 DEPS 选择与完整缓存

第 88 轮同步带 `--nohooks --noprehooks --no-history --shallow --jobs=2`，实际在 1800 秒上限被终止，`evidence/deps-sync-88.json` 保留 `dependencies_synced=false`；活跃任务包含 Git 输入，没有新的工作区外临时目录写入拒绝，不能把单个依赖已有对象或部分完成标记当成全量同步。第 92 轮只读枚举因标准库临时目录探测被完全禁写而未启动；第 93 轮仅允许构建根内 `t/` 的临时探测，继续拒绝网络和源码写入，真实 gclient 枚举得到根配置的 237 个选择：147 Git、43 CIPD、47 GCS，见 `evidence/deps-inventory-93.json`。这是根选择清单，不是递归依赖的验收证明。

预取按固定 DEPS 的 name、bucket、object、generation、大小和 SHA 匹配真实选择；各 HTTPS 范围要求精确 Content-Range 和字节数，拼合后仍核验完整 SHA。第 94、98、100 轮分别只完成 38、45、46 个对象，SSL 错误、90 秒请求超时、未启动范围和部分响应均保留，不关闭 TLS 校验或接纳残缺输入。续取重新核对旧完整文件与范围，复用记录不冒充本轮网络成功；第 103 轮仅请求最后一个范围并完成全部 47 个对象，共 687,108,624 字节，随后逐一独立复核大小、SHA、普通文件类型和只读 mode0400。报告 `evidence/gcs-prefetch-103.json` 的 SHA-256 为 `3655c972a5bffb9ea3be9266b2f30d706b945738e0a3bfa6d3ab8212600d0d4e`。预取没有解包或写入原生完成标记，使用 `-O` 禁用断言的入口负控明确拒绝。

第 106 轮通过真实 `GClient／GcsDependency` 从新缓存导入 18,015,920 字节 sysroot，总计约 8.2 秒。解释器审计实际观察到缓存读取；透传观察器保存的真实 gsutil 操作只有一次 stat、没有 cp，stat 约 3.2 秒、退出 0 且原始 stderr 为空，外层 stderr 也为空。原生摘要、大小、解包、`usr/include/stdio.h` 与原生标记核对通过，见 `evidence/gcs-import-106.json`、`data/gcs-import-106/{cache-read-events,gsutil-operations}.json`；不能把该单对象导入扩称为全量同步。

第 107 轮重新通过独立 index 的两组补丁顺序应用、逐文件比对及 HEAD／refs 检查；四文件组合 diff 仍为 `5c0821c0ae9a89fc67d3878c239cfb953c359944d186b405bdd0bb5a81ab7725`，未改变固定工具源码。封存 `evidence/depot-patch-107.json` 绑定新导入证据和 47 份缓存，旧第 89 轮仍只认证其原来的 25 份输入。构建环境现显式指向 `data/gcs-prefetch-103/cache`，环境脚本 SHA-256 为 `ef92cb4ef960a9ab2eec9624231a7b1c54451acff1541912c113bf2c42040833`；上述相对路径均位于 `target/n5/capture-toolchain-20261115/`。**全部 DEPS、pre-hooks／hooks、GN、定制引擎及生产捕获仍未完成**，截图关闭、身份 v1 与新增远端验收限制不变，N5 全范围未完成，未推送。

### 固定递归同步、hooks 与捕获构建图

2026-11-16，后续成功范围已推进到固定配方的递归依赖、hooks 与真实 GN 生成；前述第 107 轮及更早的“未完成”按各自时点保留，不再代表当前构建前置状态。以下证据均位于 `target/n5/capture-toolchain-20261115/`：

| 阶段 | 实际结果 | 证据 |
| --- | --- | --- |
| 第 164 轮递归同步 | 配置范围内 `gclient sync` 与 `revinfo --actual` 均退出 0，约 2362.46 秒 | `evidence/deps-sync-164.json` |
| 第 174 轮正常 pre-DEPS 阶段 | 去掉 `--noprehooks`，保留 `--nohooks --no-history --shallow --jobs=4`，退出 0，约 52.20 秒；七份选中递归 DEPS 无 pre-hook 声明，动作清单为空、执行数不伪填 | `evidence/deps-pre-hooks-174.json` |
| 第 177 轮内嵌 depot_tools | 固定 `b86fc981b72ab966f8260f73b4eca600b3b12677` 加与外层相同的四文件组合补丁；逐文件、完整 diff 及 Git 元数据核对 | `evidence/nested-depot-177.json` |
| 第 178 轮正式 hooks | `gclient runhooks --verbose` 退出 0，约 201.30 秒，来源与配方不变 | `evidence/deps-hooks-178.json`、`data/deps-hooks-178/hooks.log` |
| 第 180 轮捕获 GN | 23 文件固定草案已真实应用；`gn gen --fail-on-unused-args` 与 `gn desc //headless:headless_shell outputs` 退出 0，实际 38,072 targets／5,134 files | `evidence/capture-gn-180.json`、`data/capture-gn-180/command-{053,054}.{stdout,stderr}` |

配方仍为第 112 轮确认的 small checkout：`target_os=['linux']`、`target_os_only=False`、`cache_dir=None`，`custom_deps` 仅排除 `src/chrome/test/data/xr/webvr_info`，活跃根 Git 依赖为 146；不是未经定制的原始 small checkout 全选完成。第 174 轮未选中的 `crossbench-web-tests/DEPS` 含 pre-hook 声明，不能据此说本轮执行过它们。第 178 轮先核对正式 hermetic Xcode 查询为关闭；原始 hooks 日志明确跳过 Mac toolchain 安装，reclient 配置使用 `--skip_remoteexec_cfg_fetch`。配置生成不等于远端执行，未上传源码或启动 CI。

GN 输出为 `src/out/saya_capture_linux_arm64_180/`，参数严格为 `target_os="linux"`、`target_cpu="arm64"`、`is_debug=false`、`is_component_build=false`、`is_official_build=false`、`symbol_level=0`、`use_remoteexec=false`、`use_siso=false`。`args.gn` SHA-256 为 `f8c5a2809e541a9d404bb781509647a8701bb30fb9ca9411878bb04895367b5a`，`build.ninja` 为 `7599d3af43a501f796cc27d525505a5e39ff3e0b0854f3e6860a66ed10fa02b7`；GN 与固定 CPython、完整 SDK、23 文件补丁、DEPS 和 Git 元数据均前后核对。第 180 轮报告 SHA-256 为 `02a97eb348e103a0295fd99f42ce1b5235fec3f8576a30429dbd3d9dcbb43a38`。原封存补丁未修复已知绘制缺口；GN 成功不是 C++ 编译、链接、目标 std、像素遮蔽或生产捕获成功。

当前本地编译尚无通过证据，不将生成图或后续编译失败当作隐私行为 RED。写入范围继续只允许任务的构建／工具链目录及既有设备节点，全部可控缓存与临时文件位于工作区；不扩大到系统 `/tmp` 或 `/var/folders`。生产 `screenshots`／`screenshot_masking` 保持 false，native Browser、私有捕获桥接与新增四平台仍未完成，未推送或发布。

## 复跑入口

先按 [Browser 配置与 VM 前提](browser.md)显式设置全部 `SAYA_TEST_VM_*` 输入，程序身份与源码归属另行保留。

```bash
python3 -I -B scripts/n5/acceptance.py target/release/saya --evidence-dir target/n5-acceptance
```

要求真实生产截图时必须严格运行（预置定制引擎与三补丁 worker 的 `colima-n5cap` `saya-capture-fixed-265` 已实跑通过 22/22；若指向未修补引擎则会按预期 fail-closed 失败）：

```bash
python3 -I -B scripts/n5/acceptance.py target/release/saya --evidence-dir target/n5-acceptance-screenshot --require-screenshot
```

```bash
python3 -B scripts/n2/native_acceptance.py target/release/saya --benchmark-samples 3 --output target/n5/native-evidence.json
```

```bash
python3 -B scripts/n2/verify_performance.py target/n5/native-evidence.json --samples 3
```

## 已有证据之外的显式边界

1. 除已完成交叉编译与实际验证的 `linux-arm64` 定制 `headless_shell`（`501,816,112 B`，`d3c691a5…`）外，若需在非 `linux-arm64` 架构（如 `linux-x86_64`）原生启用 Masked 截图，仍需另行构建与分发对应目标架构的定制 Chromium `headless_shell`（四平台远端终验使用 Playwright rev 1254 stock Chromium，未修补引擎按设计 fail-closed 返回 `browser_capability_disabled`）。
2. 版本号保持 `0.2.0` 且未创建新正式发行标签；是否发布新版本或归档定制 Chromium 构建产物取决于项目负责人后续决定。

以上远端工作流与本机／VM 联合矩阵均仅在受控夹具与本地网页环境下验证，未使用真实外网页面或付费模型；构建准备中的官方依赖下载不算 Browser 外网验收，所有完成声明严格以实际可核查证据为准。
