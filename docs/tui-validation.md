# N1 TUI 验证记录

**记录于 2026-11-08。候选版本为 0.2.0-rc.1。本轮用于实现验收和本机试用，正式发布安排在后续代码 review 与其他测试之后。**

## 范围与证据

DEV-047 至 DEV-053 覆盖默认 TUI、共享运行控制、会话与记录、入口兼容、四平台原生交互和候选包验证。实际任务状态由[开发进度](progress.md)维护，界面结构与修改入口见[TUI 设计说明](tui-design.md)，中英双语说明见[候选说明](release-notes-0.2.0-rc.1.md)。

## 本机检查

本机为 macOS 27.2 ARM，使用 Rust 1.99.1／edition 2024，macOS 构建部署目标为15.0。0.2.0-rc.1已通过格式检查、workspace全目标Clippy、445项Rust测试与优化构建，日志为本机target/n1-rc-all-check.log。5项发行工具测试通过。最终源码提交与二进制摘要随产物补充，本机系统与最低支持环境分别记录。

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

| 目标 | 原生最低环境要求 | 状态 |
| --- | --- | --- |
| linux-x86_64 | Ubuntu22.04，glibc2.35，x86_64 | 待验证 |
| linux-aarch64 | Ubuntu22.04，glibc2.35，aarch64 | 待验证 |
| macos-x86_64 | macOS15，Intel原生，部署目标15.0 | 待验证 |
| macos-aarch64 | macOS15，ARM原生，部署目标15.0 | 待验证 |

候选工作流对解包程序执行工具smoke、文本会话、审查回归与全部TUI场景，记录归档／二进制SHA-256、源码声明、脚本摘要及实际系统。Linux检查GLIBC符号上限与共享库，macOS检查硬件／进程架构及Mach-O最低版本。产物与证据作为Actions artifacts保存14天。

## 验证边界

模型测试使用受控本地HTTP服务，真实模型行为和人机界面主观体验由后续试用补充。命令使用宿主权限，OS Sandbox、MCP和自动Compact按后续计划实施。SIGKILL、崩溃、断电或已断开的物理终端可能需要人工恢复并核查副作用，恢复命令见双语候选说明。
