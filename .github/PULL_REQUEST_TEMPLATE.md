## Summary / 概述

<!-- What changed and why. Link issues with "Closes #123".
     改了什么、为什么改。关联 Issue 请用 Closes #123 / Fixes #123。 -->

## Changes / 改动内容

<!-- List the main changes. / 主要改动点列表。

-
-->

## Verification / 验证

<!-- What you actually ran (e.g. `just check`, `just test-tooling`, manual steps) and the result.
     If something was not run, say why and what remains unverified.
     实际执行过哪些检查（如 `just check`、`just test-tooling`、手工验证步骤），结果如何。
     未执行的检查请说明原因与未覆盖范围。 -->

## Checklist / 检查清单

- [ ] `just check` passes (fmt + strict Clippy + workspace tests + optimized build) / `just check` 通过
- [ ] Change is focused, no unrelated refactors / 改动聚焦，未附带无关重构或格式化
- [ ] Docs and examples updated if behavior, interfaces, or config changed / 行为、公共接口或配置变化已同步相关文档与示例
- [ ] Decision log updated if public contracts or defaults changed (docs/decisions.md) / 涉及公共契约或默认值的改动已在 docs/decisions.md 记录
- [ ] No secrets committed; credentials referenced via env vars only / 未提交密钥、令牌或真实凭据
- [ ] Commit messages follow Conventional Commits / Commit message 遵循 Conventional Commits
