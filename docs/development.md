# 开发指南

## 当前状态

仓库已有固定 Rust 1.99.1 workspace、core 运行循环、模型适配和本地工具，CLI MVP 已发布 0.1.0。N0／N1随0.2.0交付，N2与N3已分别完成四平台固定源码验收；N3最终程序、源码清单及联合矩阵见[N3验收记录](n3-validation.md#四平台最终验收)。CLI用法见[使用指南](usage.md)。

## 开发流程

1. 阅读 [AGENTS.md](../AGENTS.md)、[开发进度](progress.md)及任务涉及的设计文档。
2. 检查当前文件与工作区修改，按对应版本的依赖和退出条件确定任务。MVP 定义见[实施任务](implementation-plan.md)，新增功能见[后续开发计划](post-mvp-plan.md)。
3. 按已确认基线实现；只有改变范围、默认值或公共契约时才补充[决策记录](decisions.md)，普通内部实现自行处理。
4. 完成聚焦、完整的改动并同步相关文档，按[路线图](roadmap.md)中的完成标准验证。
5. 在进度记录中更新状态、证据、阻碍与下一任务，报告实际结果及未验证范围。

工程与全部产品契约已由 D-027 确认，见[完整基线](mvp-baseline.md)。固定 Rust 1.99.1／edition 2024，MVP workspace 为 saya-core＋saya-cli，DEV-038 增加共享应用服务 saya-app，DEV-047 增加 saya-tui library，二进制 saya，许可证 MIT。具体依赖补丁版本由 Cargo.lock 记录。

根目录 [LICENSE](../LICENSE)、.gitignore、workspace 和工具链固定文件已建立。MVP 的实施定义为 P0 至 P7、DEV-001 至 DEV-037，新增任务为 DEV-038 至 DEV-089。各版本依据对应任务和实际验收维护状态，原 CLI／core 基线见 B-02。

提交信息遵循 [AGENTS.md](../AGENTS.md) 中的 Conventional Commits 约定。

## Just 开发入口

根目录 [justfile](../justfile) 统一提供常用开发任务。先安装固定Rust工具链及 `just`，Python工具回归另需Python3。本轮使用 `just 1.59.0` 验证。macOS可通过Homebrew安装，macOS／Linux也可通过Cargo安装。[Just官方安装说明](https://just.systems/man/en/packages.html)

```sh
brew install just
# 或通过 Cargo 安装
cargo install just --locked
```

在仓库根目录或任意子目录执行 `just` 查看任务，recipe默认在根目录运行。构建默认使用 `MACOSX_DEPLOYMENT_TARGET=15.0` 和 `CARGO_PROFILE_DEV_DEBUG=line-tables-only`，已显式设置的同名环境变量优先。测试并发由Cargo／Rust测试框架及调用者的环境设置控制。

| 命令 | 用途 |
| --- | --- |
| `just check` | 复用 `scripts/check.sh`，依次检查格式、严格Clippy、workspace测试及优化构建 |
| `just fmt`／`just fmt-check` | 格式化／只检查Rust格式 |
| `just clippy` | 全workspace、全目标严格Clippy |
| `just test` | 全workspace测试，可追加过滤条件和测试参数 |
| `just build`／`just release` | 开发／优化构建，可追加Cargo构建参数 |
| `just run` | 运行开发版Saya，可追加CLI参数 |
| `just tui-preview syntax` | 运行debug专属TUI场景预览 |
| `just render-preview 140 60 syntax` | 输出离线场景快照，可追加 `--source` 或 `--json` |
| `just test-tooling` | 执行CI使用的发行、N2与N3离线Python回归 |
| `just smoke BINARY` | 对显式指定程序执行CLI、文本会话、审查回归及完整TUI旅程 |

```sh
just test restored_flags_are_observable_before_the_child_reads_its_release -- --exact
just run --help
just tui-preview footnotes --source
just render-preview 140 60 mermaid --json
just release
just smoke target/release/saya
RUST_TEST_THREADS=4 just check
```

Recipe通过带引号的位置参数转发输入，可变参数使用 `"$@"`，smoke程序路径使用 `"$1"`。含空格、引号或shell元字符的参数保持原值。CLI内的工作区及文件相对路径以仓库根目录为起点。[Just位置参数](https://just.systems/man/en/positional-arguments.html)

`smoke`要求显式提供要验收的程序路径。使用 `CARGO_TARGET_DIR`、`CARGO_BUILD_TARGET` 或Cargo的目标配置构建时，传入其实际产物路径。构建及工具安装可按需下载依赖；Python回归使用离线替身，smoke使用本地模型夹具、loopback和Unix PTY，文件与命令场景需要当前平台的原生隔离依赖。[原生配置](configuration.md#n2-原生隔离配置)、[Cargo构建输出](https://doc.rust-lang.org/cargo/commands/cargo-build.html#output-options)

## 文档修改的验证

- 检查仓库内 Markdown 链接是否存在。
- 检查 README、范围、架构和决策之间是否一致。
- 明确区分现有能力与拟议功能。
- 命令示例应能在注明的条件下执行；尚未实现的命令不得写成使用说明。

## Rust 工程建立后的检查

运行 `just check` 通过 `scripts/check.sh` 依次执行以下检查。脚本也支持通过绝对路径调用。实际结果见[开发进度](progress.md)。

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked --no-fail-fast
cargo build --workspace --release --locked
```

测试命令会继续收集其他测试目标的结果，任一失败仍使脚本以失败状态结束。CI使用 `CARGO_PROFILE_DEV_DEBUG=line-tables-only` 保留调试行表，N2验收另行记录实际debug和优化镜像大小。

GitHub Actions 使用 Rust 1.99.1 与相同的检查脚本，配置原生 runner：Ubuntu 22.04 x86_64／aarch64（`ubuntu-22.04`／`ubuntu-22.04-arm`），macOS 15 aarch64／x86_64（`macos-15`／`macos-15-intel`）；macOS 构建设置最低部署版本 15.0，并在两种 macOS runner 执行离线工具往返与交互会话 smoke。四个 runner 均执行离线 release smoke；不自动上传构建产物。runner 可用性与结果须以实际运行记录为准；运行记录未覆盖的候选仍需独立验证。

核心使用 serde／serde_json 表达结构化协议数据，uuid 生成 UUIDv7 会话与运行标识；Tokio 用于运行时、异步 HTTP、工具与 CLI。版本在 Cargo.lock 中固定。测试使用 `tempfile` 开发依赖提供临时工作区；`crates/saya-core/tests/support` 包含辅助函数，`crates/saya-core/tests/fixtures` 为模型、核查和三协议预留离线数据目录。目录本身不表示替身或协议已实现。

CLI 功能实现后提供与 B-03 一致的安装、配置和启动说明；未构建或未 smoke 验证的产物不标记已支持。

## 只读工具开发状态

`crates/saya-core/src/tools/read.rs` 提供只读文件／目录与搜索 API、`Tool` 适配器和可配置上限。它使用 cap-std 根目录句柄、ignore／globset、regex、sha2 及 Tokio 阻塞线程池；具体版本见 Cargo.lock。搜索读取 `.gitignore` 时同样受路径根约束。工具执行前由调度和权限层授权，已接入 CLI。根检查不限制子进程，也不等同于操作系统沙箱。

`crates/saya-core/src/tools/write.rs` 实现受授权钩子保护的单文件写入与统一补丁，使用 diffy 校验单文件补丁，要求执行前记录意图。CLI 使用持久化与权限调度后执行真实工作区副作用；内存存储仅供受控测试。版本检查在替换前执行；现有文件的检查与替换之间不具备对任意外部并发写入的原子比较交换保证。

`crates/saya-core/src/tools/exec.rs` 提供直接 argv 进程适配器，依赖 Tokio process 与 nix 信号；CLI 可在权限和持久化调度后启动命令；内存存储仅供受控测试。进程组清理并不构成文件系统、网络或主动脱离进程组的后代隔离。

`crates/saya-core/src/permissions.rs` 提供权限规则解析和纯决策；`PolicyGate` 对需要人工或模型判断的操作失败关闭。`crates/saya-core/src/review.rs` 已提供与模型替身验证的独立核查组件；三协议和单次许可已接入 core，调用者共享运行预算并在派发前重查。

## 核心验证要求

Agent 循环的测试优先使用可控的模型与工具替身，覆盖直接回答、工具调用后继续、工具失败、模型失败、取消以及达到限制。测试应验证可观察行为和边界，避免绑定不必要的内部步骤。

验证四种工作模式、三种权限模式、PLAN 始终只读且禁用 exec、用户切换在调用边界生效及同会话上下文保留。覆盖默认串行、后台并发与清理、最后请求优先、确认失效、取消优先以及派发前重新检查。

三类模型协议分别验证直接回答、工具请求与工具结果的往返。核查模型使用替身，覆盖规则判断、模型核查、用户兜底和非交互无法确认的路径。

会话验证保存、列出与恢复、工具协议记录闭合、unknown、存储失败、写锁与迁移。恢复后等待输入且不重执行工具，使用本次启动权限，不恢复历史授权。按 B-17 验证流式、JSON、JSONL、事件顺序与退出码。验收夹具及用例见[需求规格](requirements.md)与 B-23。

真实模型联调单独执行，明确所需配置与可能产生的费用，不作为默认测试的隐含前提。密钥通过最终选定的配置机制提供，仓库只保留不含真实凭据的示例。

## 文档随实现更新

建立 Rust 工程后更新“当前状态”；实现 CLI 后补充真实命令、配置和退出行为；公共接口稳定后再补接口说明。尚未实现或验证的 TUI、GUI 能力继续保留为计划。

## Markdown 与 debug 预览开发

TUI的CommonMark／GFM使用pulldown-cmark与一致的Unicode脚注标签匹配，高亮使用内置Syntect语法，Mermaid使用同版本merman-core与mermansi。merman-core启用JSON对象顺序保留，并通过granit-parser约束smallvec小于1.16，实际版本由Cargo.lock记录。core规范化参数显式排序，以维持审批和拒绝记忆的稳定身份。

默认开发构建可运行 `just tui-preview syntax`，离线单元格快照使用 `just render-preview 140 60 syntax --json`。场景与源码／渲染切换见[Markdown说明](markdown-rendering.md#debug-预览)。完整Rust检查使用 `just check`，各源码的四平台同步测试范围见[开发进度](progress.md)。

## N2 原生性能证据

在具备对应原生后端的机器上对指定优化程序执行验收，使用完整单次报告核对性能字段。Linux委派资源的运行要求见[N2验收记录](n2-validation.md)。

```sh
python3 scripts/n2/native_acceptance.py target/release/saya --benchmark-samples 3
python3 scripts/n2/verify_performance.py target/n2/acceptance/native-evidence.json --samples 3
python3 scripts/n2/test_native_acceptance.py
python3 scripts/n2/test_verify_performance.py
```

报告保留原始样本、实际操作证明和CPU／RSS范围，核验器重算nearest-rank分位数及比较值。热命令复用同一已准备Run，代理RTT使用payload内部时钟，具体定义见[隔离性能说明](sandbox.md#取消恢复与验收)。

## 授权后的发行构建

[Release candidates](../.github/workflows/release-candidates.yml) 由操作者手动触发，在 Ubuntu 22.04 ARM／Intel 和 macOS 15 ARM／Intel 原生构建同一提交。各 job 执行完整 Rust 检查，生成单目标候选压缩包，并对解包程序执行工具、交互和审查回归。Linux 验证实际 glibc 2.35 与符号上限，macOS 验证实际系统主版本 15 和部署目标 15.0。

通过验证的候选、校验和及环境／二进制证据上传为 Actions artifacts，保留 14 天，供已获授权的发行操作下载核验。日常 CI 执行构建与测试。发行操作者汇总四个包的 SHA256SUMS，检查源码和二进制关联，再发布固定提交的 Git 标签与 GitHub Release。发布后的下载文件应与通过验收的包逐字节一致。


## N3 固定候选验收

先执行 `just check` 与 `just test-tooling`；本机测试并发较高时使用 `RUST_TEST_THREADS=4 just check` 控制文件系统争用。正常CLI和TUI使用显式程序SHA及源码commit运行，脚本保留实际源文件摘要、模型请求／工具配对、SQLite和终端恢复结果。三协议均使用本地受控HTTP替身。

```sh
python3 -B scripts/n3/acceptance.py /absolute/path/to/saya --expected-sha256 FULL_SHA256 --source-commit FULL_COMMIT --report target/n3/cli.json
python3 -B scripts/n3/tui_acceptance.py /absolute/path/to/saya --expected-sha256 FULL_SHA256 --source-commit FULL_COMMIT --report target/n3/tui.json
SAYA_TEST_EXECUTABLE=/absolute/path/to/saya cargo test -p saya-core --locked --test plugin_services -- --ignored --test-threads=1
```

原生插件套件必须实际执行8项，普通测试中的ignored状态不计入原生验收。GitHub的 `N3 native acceptance` 工作流按完整source_sha在四个原生runner执行源码门禁、基线、既有隔离／性能／MCP及N3联合任务，保留成功与失败证据14天。候选、源码、架构或必需步骤状态不匹配时验收失败。[逐项状态](n3-validation.md)
