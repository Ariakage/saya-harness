# Contributing / 贡献指南

[中文](#贡献指南)

Thank you for your interest in contributing to Soyo Harness. This section explains the process and conventions.

## Before you start

Please read these documents first:

- [AGENTS.md](AGENTS.md): repository-wide conventions, including scope control and Git commit rules
- [docs/development.md](docs/development.md): development workflow and the `just` task runner
- [docs/progress.md](docs/progress.md): actual completion status of planned tasks
- [docs/mvp-baseline.md](docs/mvp-baseline.md): confirmed product and engineering baseline
- [docs/decisions.md](docs/decisions.md): decision log; changes to public contracts or defaults need a new entry

## Development environment

- Rust `1.99.1` (pinned by `rust-toolchain.toml`, with `clippy` and `rustfmt`, edition 2024)
- [`just`](https://just.systems/): unified entry for development tasks
- Python 3: required for the tooling regression suite under `scripts/`
- Supported platforms: macOS 15+, Linux glibc 2.35+ (`x86_64` / `aarch64`)

## Common commands

```sh
just            # list all tasks
just check      # full check: fmt + strict Clippy + workspace tests + optimized build
just fmt        # format code
just clippy     # strict Clippy across the workspace and all targets
just test       # workspace tests
just test-tooling  # offline Python regression for release/protocol scripts
```

Make sure `just check` passes before opening a PR. If you cannot run it, explain the blocker and the unverified scope in the PR description.

## Commits and pull requests

1. Branch from `main` and keep changes focused; no unrelated refactors, dependency swaps, or reformatting.
2. Commit messages follow [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/): `type(scope): description`; mark breaking changes with `!` or a `BREAKING CHANGE:` footer.
3. When changing behavior, public interfaces, or configuration, update related docs and examples in the same change. Doc-only changes do not need the Rust check, but keep links and facts consistent.
4. Push the branch and open a PR; fill in the template with what changed, how it was verified, and what remains unverified.
5. Reviews may request changes; respond with additional commits — maintainers will tidy history at merge time if needed.

## Conventions

- Documentation and discussion default to Chinese; Rust identifiers are in English; release notes are bilingual.
- External input (model responses, tool arguments and outputs) must be handled with explicit errors — no `unwrap`/`expect`/panic as a substitute.
- Tests use controllable model and tool doubles; they must not depend on real APIs, network access, or paid quota by default.
- Credentials are referenced via environment variables only; never commit keys, tokens, or real configs.
- Clearly separate what is implemented, confirmed, drafted, and undecided; do not present plans as facts.

## Reporting issues and requesting features

- Bugs: use the [Bug Report](https://github.com/Ariakage/soyo-harness/issues/new?template=bug_report.yml) issue form
- Features: use the [Feature Request](https://github.com/Ariakage/soyo-harness/issues/new?template=feature_request.yml) issue form
- Security vulnerabilities: do **not** open a public issue; report privately via [SECURITY.md](SECURITY.md)
- For other questions see [SUPPORT.md](SUPPORT.md)

## License

By contributing, you agree that your changes are licensed under the repository's [MIT License](LICENSE).

---

# 贡献指南

感谢你愿意为 Soyo Harness 做出贡献。本文说明贡献前需要了解的流程与约定。

## 开始之前

请先阅读以下文件，了解项目定位、当前状态与协作约定：

- [AGENTS.md](AGENTS.md)：仓库总约定，包括工作模式、修改范围与 Git 提交规范
- [docs/development.md](docs/development.md)：开发流程与 `just` 任务入口
- [docs/progress.md](docs/progress.md)：各版本任务的实际完成状态
- [docs/mvp-baseline.md](docs/mvp-baseline.md)：已确认的产品与工程基线
- [docs/decisions.md](docs/decisions.md)：决策记录；涉及公共契约或默认值的改动需要同步新增条目

## 开发环境

- Rust `1.99.1`（由 `rust-toolchain.toml` 固定，含 `clippy`、`rustfmt`，edition 2024）
- [`just`](https://just.systems/)：统一开发任务入口
- Python 3：运行 `scripts/` 下工具回归时需要
- 支持平台：macOS 15+、Linux glibc 2.35+（`x86_64` / `aarch64`）

## 常用命令

```sh
just            # 列出全部任务
just check      # 完整检查：格式 + 严格 Clippy + workspace 测试 + 优化构建
just fmt        # 格式化
just clippy     # 全 workspace、全目标严格 Clippy
just test       # workspace 测试
just test-tooling  # 发行与协议脚本的 Python 离线回归
```

提交 PR 前请确保 `just check` 通过；无法执行时，请在 PR 中说明具体阻碍与未验证范围。

## 提交与 Pull Request

1. 从 `main` 拉新分支，保持改动聚焦；不附带无关重构、依赖替换或格式化。
2. Commit message 遵循 [Conventional Commits 1.0.0](https://www.conventionalcommits.org/zh-hans/v1.0.0/)：`type(scope): description`，破坏性变更标记 `!` 或用 `BREAKING CHANGE:` 脚注。
3. 修改行为、公共接口或配置时，同步更新相关文档与示例；只改文档时无需跑 Rust 检查，但请确认链接与事实一致。
4. 推送分支并开启 PR，按模板填写改动说明、验证方式与未覆盖范围。
5. 评审可能要求修改；请用追加 commit 回应，合并前由维护者处理整理历史。

## 约定要点

- 说明文档与沟通默认使用中文，Rust 标识符使用英文；发行说明中英双语。
- 外部输入（模型响应、工具参数与输出）必须显式处理错误，不以 `unwrap`/`expect`/panic 代替。
- 测试使用可控的模型与工具替身，不默认依赖真实 API、网络或付费额度。
- 凭据只通过环境变量引用；不要提交密钥、令牌或真实配置。
- 明确区分已实现、已确认、草案与待决定，不把计划写成事实。

## 报告问题与提需求

- Bug：使用 Issue 模板 [Bug 报告](https://github.com/Ariakage/soyo-harness/issues/new?template=bug_report.yml)
- 功能建议：使用 Issue 模板 [功能建议](https://github.com/Ariakage/soyo-harness/issues/new?template=feature_request.yml)
- 安全漏洞：**不要**开公开 Issue，请按 [SECURITY.md](SECURITY.md) 私密报告
- 其他使用问题见 [SUPPORT.md](SUPPORT.md)

## 许可证

贡献即表示你同意以仓库的 [MIT License](LICENSE) 授权你的改动。
