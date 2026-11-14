# N5 Browser 实现与验收

**2026-11-14：DEV-079 至 DEV-082 已提交；DEV-083 本机／专用 VM 联合阶段完成 21 passed、1 disabled，三协议 CLI／TUI 回归通过。生产截图、native Browser 及新增四平台证据仍未满足，DEV-083 全范围与 N5 不标为完成。**

任务范围来自[后续开发计划](post-mvp-plan.md#n5-browser)；实现与安全契约见 [Browser](browser.md)，对外范围见[中英双语说明](release-notes-n5.md)。

## 提交与范围

| 阶段 | 提交 | 已有实现与证据 |
| --- | --- | --- |
| DEV-079 | `5a61152` | BrowserProvider／BrowserState、受控 Web、操作描述、引用、权限与取消组件 |
| DEV-080 | `058bb9e` | 固定 worker、身份校验、生产装配、专用 VM、双层 sandbox 与生命周期 |
| DEV-081 网络阶段 | `e2587fe` | 共享 Run 网络预算、broker、反向 SSH、私有 netns、旁路 canary |
| DEV-081 凭据／文件／后台 | `f1bbc5c` | origin 凭据槽、下载／上传 artifact、GC／lease、取消与 unknown |
| DEV-082 | `1443291` | CLI／TUI 审批与产物、三协议图片、有效上下文媒体加载、来源保真与导出 |
| DEV-083 | 本次本机验收阶段 | 正常 CLI／TUI／专用 VM 联合矩阵通过；保留截图 disabled、native Browser 与新增四平台缺口，不标全范围完成 |

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

[D-068](decisions.md) 已确认允许扩展托管捕获引擎与固定身份契约，必要时采用定制 Chromium；它不是完成证明。[引擎实施边界](browser.md#托管捕获引擎扩展d-068待实现)保留预捕获遮蔽、事务屏障与双层 sandbox。固定源码的 Linux 构建文档要求至少 100 GB 空闲空间，本机 Akari 卷当前仅约 37 GiB；尚无已确认满足条件的独立构建环境。默认 Linux 工具链的原生 ARM 路径也不能据已有 VM 原语结果推定可用。

未下载完整引擎源码、未运行或部署定制构建，未清理归属不明的构建或用户缓存。生产截图继续禁用；四平台远端验收按项目负责人最新要求等待再次授权，不以旧源码、TCG 可行性调查或交叉编译代替实际宿主成功证据。

## 复跑入口

先按 [Browser 配置与 VM 前提](browser.md)显式设置全部 `SAYA_TEST_VM_*` 输入，程序身份与源码归属另行保留。

```bash
python3 -I -B scripts/n5/acceptance.py target/release/saya --evidence-dir target/n5-acceptance
```

要求真实生产截图时必须严格运行；当前固定 worker 预期不能满足该要求：

```bash
python3 -I -B scripts/n5/acceptance.py target/release/saya --evidence-dir target/n5-acceptance-screenshot --require-screenshot
```

```bash
python3 -B scripts/n2/native_acceptance.py target/release/saya --benchmark-samples 3 --output target/n5/native-evidence.json
```

```bash
python3 -B scripts/n2/verify_performance.py target/n5/native-evidence.json --samples 3
```

## 尚未满足的退出条件

1. 固定生产 Browser 的可靠预捕获遮蔽及真实截图成功证据。
2. 当前 N5 源码的 native Browser 与新增四平台固定源码联合／发行证据；既有 N2／N3 四平台报告不得移作本轮证据。

本轮未推送、未创建发行、未使用真实外网或付费模型；版本号未变。完成声明以实际证据为准，不以模型文字或安全拒绝代替尚缺功能。
