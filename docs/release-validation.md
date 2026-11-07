# 0.1.0 发行候选验证记录

**状态：待验证，尚非可发布版本。** 本记录对应本地候选的源码提交 `a1b1b02d9fcc789b3f1634c4bdf654fd6e01c45a`，不是本文所在的后续文档提交。候选包仅在本机 `target/release-candidate-a1b1b02/`，不随 Git 提交；`target/p7-evidence/` 保存 Linux 构建与 smoke 日志。候选未经签名、公证或外部发布。

## 平台与验证范围

| 目标 | 构建与运行环境 | 已观察证据 | 待补充 |
| --- | --- | --- | --- |
| Linux aarch64 | Ubuntu 22.04 用户空间、glibc 2.35；ARM 虚拟机 | release 构建，二进制架构，最高 GLIBC 符号版本 2.34；离线工具往返与使用流程 smoke 通过 | 更广泛的 Linux 环境及真实模型质量不由此证明 |
| Linux x86_64 | Ubuntu 22.04 用户空间、glibc 2.35；ARM 虚拟机中经 Rosetta 模拟 | release 构建，二进制架构，最高 GLIBC 符号版本 2.34；离线工具往返与使用流程 smoke 通过 | 未在原生 Intel 主机运行本次候选 |
| macOS aarch64 | macOS 27.2 ARM，SDK 27.0 | release 构建与 smoke 通过，Mach-O `minos 13.0` | **尚未在 macOS 13 运行** |
| macOS x86_64 | macOS 27.2 ARM 上交叉构建，Rosetta 执行 | release 构建与 smoke 通过，Mach-O `minos 13.0` | **尚未在 macOS 13 或原生 Intel 主机运行** |

Linux 容器复用虚拟机内核，不代表在所有 Linux 内核上运行；仅检查 ELF 依赖的最高 GLIBC 符号不能代替所有依赖、内核及运行时兼容性验证。macOS 的部署目标声明不是 macOS 13 smoke 证据，尤其当前 SDK 比最低运行环境新。

离线 smoke 使用临时工作区和本地 HTTP 模型替身，实际执行 PLAN 文件读取及工具结果回传，检查工作区不变。独立流程演练配置检查、交互会话、人工批准读取、退出、列出会话、恢复等待输入和继续任务，并验证恢复历史包含原工具结果。四平台候选二进制均通过两项演练；x86_64 的本次执行均使用模拟。人工批准由测试脚本控制伪终端，不能替代真实用户可用性验收。

## 构建与校验

- Linux 使用 Ubuntu 22.04 镜像及 Rust 1.99.1，`cargo build --workspace --release --locked`；可复核的容器构建模板见 [ubuntu-22.04.Dockerfile](../scripts/release/ubuntu-22.04.Dockerfile)。本次 arm64 通过多架构索引 `sha256:076e7bf82319619f28dd0aa9c02ab34b055ad37100395b4f49a2ffba04fb5e11` 解析为 ARM 镜像，amd64 使用架构清单 `sha256:8a2321a92b438238f93b4346fff61bc4259c99fdd4d9b52ee86f153fee0c344f`。APT 软件包和 rustup 安装器未按内容固定，因而这不是完全可复现构建。
- macOS 使用 Xcode 27.1、SDK 27.0 和 Rust 1.99.1，显式设置 `MACOSX_DEPLOYMENT_TARGET=13.0` 后分别构建两个 Rust 目标。每个 Mach-O 的架构和最低部署版本已用 `file`、`otool` 检查。
- [package.py](../scripts/release/package.py) 接受四个显式二进制路径与完整源码提交，检查 ELF／Mach-O 架构、创建全新输出目录、生成四个压缩包和 `SHA256SUMS`。包内包括 `saya`、源码提交的 README 和 MIT 许可证、声明源码与二进制哈希的 `BUILD-INFO.json`。声明的源码提交不等于可独立证明的构建来源或可复现构建。
- 四个包的校验和、包内成员及二进制哈希均已复核；macOS smoke 还对实际解包的二进制执行，Linux 包内二进制哈希与已执行 smoke 的构建产物一致。可用 `shasum -a 256 -c SHA256SUMS` 在候选目录复核。

```text
2a02cc5d6bf14340477b41bc7c0dc995435d852e1523af61db6904721a58b25e  saya-0.1.0-linux-x86_64.tar.gz
8de8927b6736fc201ce3bf60a317370c7159086cbf9da82623b2da63e1fa1540  saya-0.1.0-linux-aarch64.tar.gz
c76e3e2b663559eaf7b30469367de6e2fa7ac70dde8159e4b754eb7a9a33555b  saya-0.1.0-macos-x86_64.tar.gz
f7ed458ecd3d3549d7a072a98907bb8f262f4d69b0813485d78897490407d816  saya-0.1.0-macos-aarch64.tar.gz
```

验证脚本是 [smoke.py](../scripts/release/smoke.py) 和 [journey.py](../scripts/release/journey.py)，参数均为候选二进制路径，需要 Python 3 和本地 loopback。它们不调用真实模型或读取用户凭据。[真实模型联调](live-model-validation.md)另记。

P6 源码提交 `a58da26` 的 [GitHub Actions 运行 42369396145](https://github.com/Ariakage/saya-harness/actions/runs/42369396145) 在 Ubuntu 22.04 和 macOS 15 的 ARM／Intel 四项通过。该 CI 不包含本候选之后的文档及辅助脚本，也不等同 macOS 13 验证。

## 发行前未关闭项

1. 在 macOS 13 aarch64 和 x86_64 分别运行对应候选的 smoke 与使用流程，记录实际系统版本、架构、哈希和结果；如只有单一架构，不得把另一架构标为最低环境已验证。
2. 汇总 B-01 至 B-24、AC-01 至 AC-24、已知限制及最终发布检查；补齐候选产物的版本说明及准确的支持范围。只有所有退出条件满足后才可标记 P7 完成。
3. 外部发布、推送和签名需遵循当时的用户指令；本记录本身不授权发布。
