# N1 TUI 验证记录

**更新于 2026-11-09。历史候选为 0.2.0-rc.1，RC1 之后的开发线按各节源码及实际环境分别记录。正式发布另行安排。**

## 2026-11-09 审查后的当前程序

代码提交 `727832bdc569ac23e8a7b61afe886d7d6c60f989` 修复六项确认问题，其中 TUI 包含终端错误与运行任务异常交错时的后台清理，以及异步初始化／交接失败覆盖新草稿。修复由未参与对应实现的 Agent 交叉复核，142 项 TUI lib 测试通过，含真实后台进程与 panic、两类草稿失败回归。

完整本机检查通过格式、workspace 全目标严格 Clippy、597 项 Rust 测试和 release 构建。同一优化程序通过 5 项发行工具单元测试、工具 smoke、文本 journey、既有审查回归及实际 20 组正常入口 PTY。场景涵盖三协议、4 模式×3 权限、Thinking、Todo、工具摘要／折叠、审批／澄清、补全、Tab、Ctrl+P、粘贴、鼠标、缩放、20×6、恢复、取消、慢消费者和信号终端还原。

平台为 macOS 27.2（26B123）ARM、Rust 1.99.1、Xcode 27.1、SDK 27.0，构建设置和实际 Mach-O 最低部署版本均为 15.0。程序 SHA-256 为 `80cb4093bddb9db55f000ca691ff29cefc2a9e5c87028c272a7a55d6db91d417`，执行各脚本前后核对一致。报告为 `target/review-tui-journey.json`，环境、脚本哈希、源码匹配和完整日志位于 `target/review-20261109/`。[审查与剩余边界](review-2026-11-09.md)

本轮证据限于本机与受控模型，当前开发线的其他平台、macOS 15 及真实服务仍待补充。下方 RC1 `a6c172d` 的四平台验收及其他开发线记录保留原有归属。

## 范围与证据

DEV-047 至 DEV-053 覆盖默认 TUI、共享运行控制、会话与记录、入口兼容、四平台原生交互和候选包验证。实际任务状态由[开发进度](progress.md)维护，界面结构与修改入口见[TUI 设计说明](tui-design.md)，中英双语说明见[候选说明](release-notes-0.2.0-rc.1.md)。

## 本机检查

本机为 macOS 27.2 ARM，使用 Rust 1.99.1／edition 2024，macOS 构建部署目标为15.0。源码 `a6c172d4032ec134172c117bdd19fb0acb39a74f` 的0.2.0-rc.1已通过格式检查、workspace全目标Clippy、445项Rust测试与优化构建，最终日志为本机 `target/n1-final-check.log`。5项发行工具测试通过，本机系统与最低支持环境分别记录。

本机候选位于 `target/tui-candidate-a6c172d/`，解包程序完成工具smoke、文本会话、5项审查回归与13组TUI场景。归档成员、包内外二进制、源码声明、版本及Mach-O15.0部署目标均已核对，已配置测试凭据精确值检查通过。证据为同目录的 `local-verification.json`、`tui-journey.json` 与实际110×32终端文本预览。

| 本机对象 | SHA-256 |
| --- | --- |
| `saya-0.2.0-rc.1-macos-aarch64.tar.gz` | `9ca4b75f3823da881c0b42821a9723fb0bb8663958a78598988249d76147c486` |
| 解包 `saya` | `facd68f0fe189c1b0d541c28cbd9a91f91e48b7814227da3cf2e7b2c568cdebb` |

## 原生 CI 修复

首轮Linux ARM检查发现测试将C字符缓冲固定为 `i8`，已改为 `libc::c_char`。Linux x86_64 的满终端恢复测试发现Ratatui析构在恢复blocking文件标志后输出诊断并阻塞，已由私有析构sink和公开draw拒绝恢复后调用的回归修复。另将PTY观察器的后续输入门槛修正为数据库完成和界面最终状态同时成立。旧失败或取消运行分别保留在Actions历史中。

[CI 42407739097](https://github.com/Ariakage/saya-harness/actions/runs/42407739097) 已在Ubuntu22.04的ARM／x86_64和macOS15的ARM／Intel全部通过。四个job均针对 `a6c172d4032ec134172c117bdd19fb0acb39a74f` 执行完整workspace检查与13组TUI验收，Linux两项平台修复已在原生环境复核。

`./scripts/check.sh` 执行格式、workspace Clippy、全部 Rust 测试与优化构建。发行工具包含3项打包测试和各1项 Linux／macOS 验证器测试。默认测试使用本地协议夹具和临时工作区。

## 真实 PTY 场景

[tui_journey.py](../scripts/release/tui_journey.py) 通过实际 `saya` 入口运行13组场景。

- 无子命令的非TTY／JSON／JSONL预检，确认用法错误、空stdout和存储零副作用。
- 三协议流式往返，Unicode、Ctrl+J与多行bracketed paste到实际模型请求的保真，窗口缩放后首次Ctrl+D，以及完整终端恢复。
- PLAN／BUILD／CODE／WORK与auto／manual／bypass的12组组合。
- Manual Approval覆盖读取、搜索、后台启动、后台状态和澄清，显式完整工具导出。
- 待批准写入期间切换PLAN，旧批准失效，stale_approval回传并继续，工作区保持只读。
- 会话新建／浏览／竞争锁／恢复与不重放，以及含空格路径的导出。
- Ctrl+C、断流后的继续输入、后台命令取消、进入PLAN的清理和unknown记录展示。
- 512片高频流的最终文本、慢终端消费者、SIGTERM与SIGHUP的退出和还原。

Darwin PTY在首次输出及canonical/raw切换时会增加内核状态。观察器先完成相同预热再保存基线，后续精确比较全部termios字段与F_GETFL。单元层另外验证36轮残缺输入／满队列panic、12轮快速缩放、独立stdin和持续慢读的250ms单帧I/O预算。

## 四平台候选

| 目标 | 实际原生验收环境 | 归档 SHA-256 |
| --- | --- | --- |
| linux-x86_64 | Ubuntu 22.04，glibc 2.35，x86_64 | `d06b06335bc515e735af382c4ba5394c51174549e048b42f1a3b135daaa5464b` |
| linux-aarch64 | Ubuntu 22.04，glibc 2.35，aarch64 | `e2a72096c60b22527353365b381601b696efe83ce6dc1573bb35be6042f410ae` |
| macos-x86_64 | macOS 15.7.9，x86_64，minos 15.0 | `e5178fe2c4fda565854a4283c65fb17226431e73bca32130ff778182ae18d78d` |
| macos-aarch64 | macOS 15.8.2，arm64，minos 15.0 | `7c30ebf7df4fefdb718146e22c055ef99dc1dacc73e30b319acb0e31979e64ae` |

[候选工作流42407739653](https://github.com/Ariakage/saya-harness/actions/runs/42407739653)的四个原生目标全部通过。四个包已回下载到本机 `target/n1-four-platform-a6c172d/`，逐一核对归档成员、权限、归档及二进制摘要、源码声明、测试脚本摘要和已知测试凭据。汇总证据为同目录 `release-evidence.json`。

工作流对解包程序执行工具smoke、文本会话、审查回归与全部TUI场景，记录归档／二进制SHA-256、源码声明、脚本摘要及实际系统。Linux检查GLIBC符号上限与共享库，macOS检查硬件／进程架构及Mach-O最低版本。产物与证据作为Actions artifacts保存14天。

## 验证边界

模型测试使用受控本地HTTP服务，真实模型行为和人机界面主观体验由后续试用补充。命令使用宿主权限，OS Sandbox、MCP和自动Compact按后续计划实施。SIGKILL、崩溃、断电或已断开的物理终端可能需要人工恢复并核查副作用，恢复命令见双语候选说明。

## RC1之后的运行信息与交互完善

2026-11-08，开发线在参考布局和正文配色基础上加入 `todo_write` 会话任务清单、可读Thinking卡片、长工具折叠及鼠标分区滚动。新功能以[开发进度](progress.md#任务清单thinking与可折叠工具)记录实际状态，交互契约见[TUI](tui.md#thinking任务清单与折叠)。

macOS27.2 ARM／Rust1.99.1通过部署目标15.0的完整格式、workspace全目标Clippy、562项Rust测试及优化构建。独立复核包括111项TUI lib、7项终端和41项runtime测试，新增真实进程panic／abort清理、最终Tasks快照及异常句柄消费回归。

优化程序通过18组正常入口PTY，包含三协议明文推理／opaque隔离、默认折叠与鼠标／Ctrl+O展开、单call工具卡、正文伪标题、左右滚轮、逐次Todo批准、PLAN拒写、24行侧栏和SQLite恢复。观察器等待完整UTF-8／控制序列帧，supervisor收尾持续有界排空，终端termios／文件标志／屏幕恢复门槛继续核对。5项发行工具测试、smoke、文本journey及5项审查回归通过。

- 完整日志为 `target/tui-interactive-check-final.log`，PTY报告为 `target/tui-interactive-release-matrix.json`。
- 二进制SHA-256为 `9050acb2510c02e698f199a7aaf3dbb17a4151a46b12e9e60193ed91fb2ea76b`，实际Mach-O部署目标15.0。
- 两张PNG由该程序的受控PTY单元格快照渲染，分别为 `target/tui-interactive-thinking-tasks-160x40.png` 和 `target/tui-interactive-expanded-tool-110x36.png`。

本节证据来自本机和本地协议替身，RC1的四平台原生候选验收按上方固定源码记录。

## 输入区与摘要卡片美化验收

2026-11-08，macOS27.2 ARM／Rust1.99.1，部署目标15.0。完整检查通过格式、workspace全目标Clippy、577项Rust测试和优化构建。41项view测试与8项摘要测试覆盖实际状态、512字节／120grapheme限制、未知数据回退、完整原文展开、输入占位／真实草稿、字重、键帽、Unicode多行、极小窗口、审批及粘贴警示。

优化程序完成19组正常入口PTY，新增摘要组按后续模型请求中的原始ToolResult逐卡核对展开JSON，失败状态与截断颜色由实际结果验证。既有三协议、4模式×3权限、空Tab、补全、会话恢复、导出、取消、后台任务、慢消费者和终端恢复继续通过，输入区追加同名真实占位文本和66KiB粘贴门槛。5项发行工具测试、工具smoke、文本journey和5项审查回归通过。

- 完整检查为 `target/tui-beautify-check-final.log`，正常入口报告为 `target/tui-beautify-release-matrix.json`。
- 二进制SHA-256为 `aef46edff56e04056f79cf0f2a58636e5ddf349f9fead6a82ce6d1b54865dade`，实际Mach-O部署目标15.0。
- 组件合成预览为 `target/tui-beautify-refined-160x40.png`、`target/tui-beautify-refined-110x36.png` 与 `target/tui-beautify-composer.png`，通过TestBackend单元格及Modifier生成颜色／字重投影。
- 正常入口PTY快照前缀为 `target/tui-beautify-idle-composer-80x24`、`target/tui-beautify-thinking-tasks-160x40`、`target/tui-beautify-expanded-tool-110x36` 和 `target/tui-beautify-tool-summaries-160x45`，均保存cells JSON供复核。

证据范围为上述本机和本地协议替身，平台候选记录按各自源码和产物维护。

## 四模式主题色验收

2026-11-08，本机macOS27.2 ARM／Rust1.99.1，macOS部署目标15.0。完整格式、workspace全目标Clippy、579项Rust测试和优化构建通过，独立128项TUI lib复核通过。四色映射、未知模式、常规／侧栏／极小窗口、详情单行输入、pending目标与实际生效及底栏模型前景均有组件回归。

同一程序完成19组正常入口PTY，验证12种模式权限启动、空Tab四色循环、当前与待切换文字各自配色、输入条保持当前模式色直到调用边界后同步更新，以及审批／任务语义色。5项发行工具测试、工具smoke、文本会话与5项审查回归通过。

- 完整检查为 `target/tui-mode-colors-check.log`，PTY报告为 `target/tui-mode-colors-release-matrix.json`。
- 程序SHA-256为 `d89d5ca61db05da5d564fff28b51957b9090aa4052e9138365d1c09316cb289a`，实际Mach-O部署目标15.0。
- 本轮5份单元格快照为 `target/tui-mode-colors-{work,code,plan,build,pending-work-code}-110x32-cells.json`，四色局部预览为 `target/tui-mode-colors-composers.png`。

本节采用本机和本地受控协议服务，预览由实际PTY单元格投影生成，平台候选记录按其对应源码和程序维护。

## Ctrl+P 弹出命令面板验收

2026-11-08，本机macOS27.2 ARM／Rust1.99.1，macOS部署目标15.0。完整格式、workspace全目标Clippy、590项Rust测试及优化构建通过，独立139项TUI lib复核通过。新增4项视图与7项控制器回归，覆盖居中几何、选择计数、可见窗口与命中区域、编辑光标、缩放与极小窗口、草稿／详情保留、新按键边界，以及异步详情覆盖下的审批与澄清。

最终优化程序通过20组正常入口PTY。新增组验证实际圆角浮层、高亮与24项滚动、隐藏斜杠草稿、编辑／粘贴接管、带参数命令、鼠标选择与滚轮、窗口外指针边界、普通／宽屏／20×6与缩放、详情恢复、人工审批和全局退出清理。既有三协议、模式权限、任务清单、折叠、会话恢复、取消与终端还原同时通过。5项发行工具测试、工具smoke、文本会话与5项审查回归通过。

- 完整检查为 `target/tui-command-palette-check.log`，独立复核为 `target/n1-command-modal-review.log`，PTY报告为 `target/tui-command-palette-release-matrix.json`。
- 程序SHA-256为 `094562477a5e9643727213a7276861dd884acb79850f53b745ea7fa8d9d08421`，实际Mach-O部署目标15.0。
- 生产组件合成预览为 `target/tui-command-palette-{110x32,160x40,20x6}.png`，颜色与字重来自TestBackend单元格投影。
- 正常入口PTY快照前缀为 `target/tui-command-palette-{centered-110x32,wide-160x45,tiny-20x6,over-approval-160x45}`，分别保存 `-cells.json` 和 `.png`。命令弹层专项另复跑通过，日志为 `target/tui-command-palette-release-focused.log`。

本节采用本机和本地受控协议服务，四平台候选记录按其对应源码和程序维护。
