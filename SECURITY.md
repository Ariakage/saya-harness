# Security Policy / 安全政策

[中文](#安全政策)

## Supported versions

The project is in the 0.x stage; only the latest release receives security fixes. Check your version with `soyo --version` and upgrade before reporting when possible.

| Version | Status |
| --- | --- |
| Latest release | Receives security fixes |
| Older releases | Not fixed individually — please upgrade |

## Reporting a vulnerability

**Report privately through GitHub — do not open public issues, PRs, or discussions with details.**

Go to the repository → **Security** tab → **Report a vulnerability** ([Private Vulnerability Reporting](https://github.com/Ariakage/soyo-harness/security/advisories/new)).

Please include:

- Affected version, platform, and reproduction steps
- The component involved (e.g. sandbox, MCP, credentials, browser channels)
- Potential impact and exploitation preconditions
- A proof of concept or patch suggestion if available

## Response

- Reports are acknowledged and assessed; confirmed vulnerabilities are fixed with priority and credited in the release notes (unless you prefer to stay anonymous).
- Keep details confidential until a fix is released; coordinate disclosure timing in the private report if needed.
- Reports that do not constitute vulnerabilities (e.g. issues limited to test doubles, or scenarios requiring pre-existing full local control) will be closed with an explanation.

## Security boundary notes

- The project provides native isolation by design (Bubblewrap + Seccomp + socat on Linux, a Seatbelt backend on macOS); credentials are injected via environment variables and never written to configs or logs. Bypasses of these boundaries are treated as vulnerabilities.
- Supply-chain issues in dependencies should also be reported privately; citing a CVE/GHSA ID is sufficient — no exploit needed.

---

# 安全政策

## 受支持的版本

本项目仍处于 0.x 阶段，仅最新发布版本接受安全修复。请通过 `soyo --version` 确认你运行的版本，并在报告前尽量升级到最新 Release。

| 版本 | 状态 |
| --- | --- |
| 最新 Release | 接受安全修复 |
| 更早版本 | 不单独修复，请升级 |

## 报告漏洞

**请通过 GitHub 私密漏洞报告提交，不要开公开 Issue、PR 或在 Discussion 中披露细节。**

入口：仓库页面 → **Security** 标签 → **Report a vulnerability**（[Private Vulnerability Reporting](https://github.com/Ariakage/soyo-harness/security/advisories/new)）。

报告中请尽量包含：

- 受影响的版本、平台与复现步骤
- 触发路径涉及的组件（如沙箱、MCP、凭据、浏览器通道等）
- 潜在影响与利用条件
- 如方便，附上概念验证或补丁建议

## 响应约定

- 收到报告后会确认收讫并评估有效性；确认为漏洞后优先修复，并在修复版本的 Release Notes 中说明（除非报告者要求匿名）。
- 在修复发布前请对细节保密；如有提前披露的需要，请先在私密报告中与维护者协商时间线。
- 不构成漏洞的报告（如仅影响测试替身、需要本地已有完全控制权的场景）会说明理由后关闭。

## 安全边界提示

- 本项目按设计提供原生隔离（Linux Bubblewrap + Seccomp + socat，macOS SeatbeltBackend），凭据经环境变量注入、不写入配置与日志。绕过这些边界的报告按漏洞处理。
- 依赖供应链问题请同样走私密报告；公开提交 CVE/GHSA 编号即可，无需附带 exploit。
