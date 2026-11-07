# 0.1.0 发行验证记录

**0.1.0 已发布，P7 验收完成。** 标签 `v0.1.0` 固定到源码 `eaade53649ffffa013e2503b894825e1b72f3dc1`，运行时代码、提示词和 Cargo 依赖与审查修复 `854be1b` 内容一致。

## 正式发行与下载复核

[GitHub Release](https://github.com/Ariakage/saya-harness/releases/tag/v0.1.0) 于 2026-11-07 发布，状态为正式版和 Latest。四个平台安装包、`SHA256SUMS`、`release-evidence.json` 与中英双语 `RELEASE-NOTES.md` 共七项资产已公开上传。Release 正文与附件中的双语说明一致。

[正式构建与验收](https://github.com/Ariakage/saya-harness/actions/runs/42383135600) 在四个原生架构分别通过格式检查、Clippy、342 项 Rust 测试、四项发行工具测试和 `saya 0.1.0` 版本断言。工作流对每个包解出的二进制执行工具往返、交互会话恢复及五项审查回归。

| 目标 | 实际原生验收环境 | 正式包 SHA-256 |
| --- | --- | --- |
| linux-x86_64 | Ubuntu 22.04，glibc 2.35 | `db4c419736a33b114b58783b57c41fc2f12a59d76ac902a163837bdbbe88e887` |
| linux-aarch64 | Ubuntu 22.04，glibc 2.35 | `2142de68b382c0bb8293d90d9674ef8cc5205118fe51bec26cc82572c0562973` |
| macos-x86_64 | macOS 15.7.9 | `30767ac2194ba2d8fdca9ca3ce30119928a8bae126877efe77f9b877802d3117` |
| macos-aarch64 | macOS 15.8.2 | `188af0747e24bb3c1f9f96002d9e1917d001f9b8c24c61866d9411ec7ef3cd52` |

四个包的源码声明、二进制哈希、归档成员、执行权限和测试脚本哈希均与工作流记录一致。macOS 部署目标为 15.0，Linux 的最高 GLIBC 符号版本均为 2.34。逐目标环境、包与二进制哈希和演练结果已作为 [release-evidence.json](https://github.com/Ariakage/saya-harness/releases/download/v0.1.0/release-evidence.json) 随正式版公开保存。

七项正式资产全部回下载到本机 `target/release-0.1.0-download-check/`，逐字节比较通过，GitHub 返回的资产 SHA-256 也全部一致。四个安装包通过 `shasum -a 256 -c SHA256SUMS`，远端附注标签解引用到上述固定源码，公开 Release 页面返回 HTTP 200。

回下载的两个 macOS 程序在 macOS 27.2 ARM 主机再次通过版本断言和三项演练，Intel 程序经 Rosetta 执行。macOS 15 的最低环境证据来自正式构建的原生 job。ARM 包带链接器 ad-hoc 签名，Intel 包未签名，两个包均未作开发者身份签名或 Apple 公证。

发布目录和归档成员经过本地测试凭据精确值检查，未发现该凭据。凭据文件继续位于 Git 忽略的私有目录。完整能力与已知限制见[中英双语说明](release-notes-0.1.0.md)。

## P7 最终候选验收

本节验收源码为 `2840cbbd52bc7cd03d8953c1837f43540928a290`，对应正式发布前的候选包。

[Release candidates 42381963924](https://github.com/Ariakage/saya-harness/actions/runs/42381963924) 在四个原生目标全部通过。每项执行格式检查、Clippy、342 个 Rust 测试与 release 构建，随后对压缩包内的程序执行 PLAN 工具往返、人工批准与会话恢复、五项审查回归。三个发行工具测试组共四项通过。

| 目标 | 实际最低环境 | 原生架构 | 验收包 SHA-256 |
| --- | --- | --- | --- |
| linux-x86_64 | glibc 2.35 | x86_64 | `da806437c64fb5754ab20199fe8d05214708f4b40952bcb04178f1131bd310f5` |
| linux-aarch64 | glibc 2.35 | aarch64 | `b2a27e7fb56f31b049957c89b3937aa4da76dd01f998de2c0e50251b19b01ed6` |
| macos-x86_64 | 15.7.9 | x86_64 | `ce5e4a50b3028658233b78cb21a87ed3f756d7ec3b5c1d67b58dcca33546e2c2` |
| macos-aarch64 | 15.8.2 | arm64 | `68b9ae6bf2d0f6452cbc4a67ff58cfc002e9a328fa0a2b71fe9795e05f53752e` |

下载到本机 `target/p7-acceptance-2840cbb/verified/` 的四个包逐一核对包哈希、二进制哈希、源码声明、测试脚本哈希、文件成员和执行权限，与各 job 保存的 `verification.json` 一致。完整汇总为同目录 `release-evidence.json`。正式构建已从包含最终双语说明的固定提交重新生成，通过相同验收并发布，见上方记录。

审查修复后的三协议真实模型复测共六项通过，版本和服务样本见[真实模型记录](live-model-validation.md)。B-01 至 B-24 与 AC-01 至 AC-24 的覆盖见[发行检查](release-checklist.md)和[验收索引](acceptance.md)。

## 历史本地候选 854be1b

以下记录对应审查修复后的首次本地四目标构建。该次缺少的 macOS 15 原生证据已由上方同运行时代码的最终候选补齐。

### 当时的本地候选

候选目录为本机 `target/release-candidate-854be1b/`。四个二进制均从同一个 Git 源码导出构建，导出 tar 的 SHA-256 为 `a5236a6690bd250147ea6145366dc629d150a81b0b71df86c71475da802819ca`。Linux 验证环境中的 144 个源码文件逐一与该导出核对。包内 `BUILD-INFO.json` 的源码声明和二进制哈希均已核对。

| 目标 | 构建环境 | 实际验证对象与结果 | 待补充 |
| --- | --- | --- | --- |
| Linux aarch64 | Ubuntu 22.04 ARM 用户空间，Rust 1.99.1 | 解包二进制在 ARM 虚拟机的 glibc 2.35 环境通过三项离线演练，最高 GLIBC 符号 2.34 | 更广泛内核与发行环境 |
| Linux x86_64 | Ubuntu 22.04 ARM 交叉构建，Rust 1.99.1，GCC x86_64 交叉工具链 | 解包二进制在 Ubuntu 22.04 x86_64 用户空间经 QEMU 通过三项离线演练，glibc 2.35，最高 GLIBC 符号 2.34 | 原生 Intel 主机 |
| macOS aarch64 | macOS 27.2 ARM，Xcode 27.1，SDK 27.0，部署目标 15.0 | 解包二进制在 macOS 27.2 ARM 原生通过三项离线演练，Mach-O `minos 15.0` | **macOS 15 实测** |
| macOS x86_64 | 同一 macOS ARM 主机交叉构建，部署目标 15.0 | 解包二进制在 macOS 27.2 经 Rosetta 通过三项离线演练，Mach-O `minos 15.0` | **macOS 15 实测及原生 Intel 主机** |

三项演练为 [smoke.py](../scripts/release/smoke.py)、[journey.py](../scripts/release/journey.py) 和 [review_regressions.py](../scripts/release/review_regressions.py)。它们分别验证 PLAN 工具往返与工作区不变、干净配置／人工批准／退出／会话列表／恢复／继续输入，以及 R-01 至 R-05 的真实 CLI 回归。测试使用本地模型替身、合成凭据及临时工作区。

包校验和、归档成员、二进制架构、源码声明和包内外二进制一致性均已核对。实际测试使用解包文件，测试后再次核对其哈希。源码、环境和脚本哈希及结果位于本机 `target/review-854be1b/evidence/`，主要文件为 `build-environment.json`、`artifacts.json`、四个 `*-verification.json` 及构建／演练日志。

Linux x86_64 的初次 QEMU 编译在 Cargo 父子进程 futex 等待处停滞，该尝试已停止。最终二进制使用上表的 ARM 交叉构建，并在 x86_64 用户空间实际执行。Linux 容器共享虚拟机内核，GLIBC 符号检查的覆盖范围限于记录的符号依赖。APT 包及 rustup 安装器沿用获取时内容，完整可复现构建仍待独立验证。

macOS ARM 二进制带有链接器生成的 ad-hoc 签名，Intel 二进制未签名。当前包尚未进行开发者身份签名、公证或发布。

### 包 SHA-256

```text
6bd3df8ded69d13141e485092e46a5a1e2bffc405a41e8ed75900517d728fc31  saya-0.1.0-linux-x86_64.tar.gz
cce4b4a8718e17dc290150bc38ba20ab4e3d0634064eaa1bcd880e61cdc1f368  saya-0.1.0-linux-aarch64.tar.gz
8f42fcece37890d353a3367d1a031c415e97b2162fbe6bb97eb2215c09de2d48  saya-0.1.0-macos-x86_64.tar.gz
cee5ffb625a46f029cc0ab96a844aa1ee85a4c6b888034d18e1a5cb42d0bdd4a  saya-0.1.0-macos-aarch64.tar.gz
```

### 二进制 SHA-256

| 目标 | 包内 saya |
| --- | --- |
| linux-x86_64 | `183181415aeddc40e29ec523068344f83867462a90fa927e5ff1ec43f7453ab3` |
| linux-aarch64 | `d44875422573f096413b6121d9ce015ecc2d479494438d28c1cba2f2839336f6` |
| macos-x86_64 | `2675268e400a6ac6a1fa7fc8a29894393944d4c163208dd0c8dc05e8ed543bcd` |
| macos-aarch64 | `03135fa43b1463a3108db6135f663b4a4a818ed5f6a3be4c0b83d89843c19031` |

### 当时的最低环境缺口

[GitHub Actions 42373322880](https://github.com/Ariakage/saya-harness/actions/runs/42373322880) 已再次通过只读查询核实，源码为 `47452e6173397d7b7b7eacd035116489bb2a3a70`，Ubuntu 22.04 ARM／Intel 与 macOS 15 ARM／Intel 四个 job 均成功。该 CI 运行中的二进制来源为该提交，原工作流执行离线工具往返与交互演练，未上传产物。当前候选来源为 `854be1b`，其 macOS 15 实测证据仍待补齐。

macOS 15 机器上使用 Python 3 运行下列命令，替换包路径并选择尚不存在的输出文件。脚本检查实际系统版本、包／二进制哈希、架构和部署目标，并对解包二进制执行上述三项演练。

```sh
python3 scripts/release/verify_macos15.py /path/to/saya-0.1.0-macos-aarch64.tar.gz --sha256 cee5ffb625a46f029cc0ab96a844aa1ee85a4c6b888034d18e1a5cb42d0bdd4a --output macos15-854be1b-aarch64.json
python3 scripts/release/verify_macos15.py /path/to/saya-0.1.0-macos-x86_64.tar.gz --sha256 8f42fcece37890d353a3367d1a031c415e97b2162fbe6bb97eb2215c09de2d48 --output macos15-854be1b-x86_64.json
```

ARM 主机使用 Rosetta 验证 Intel 包时记录模拟情况。验证脚本的模拟单元测试只证明检查逻辑。完成实际最低环境验证后，再复核 B-01 至 B-24、AC-01 至 AC-24 和最终版本说明，判断 DEV-031／DEV-032 及 P7 能否结束。

## 历史候选 macOS 部署目标 15.0

以下包来自审查前源码 `a1b1b02d9fcc789b3f1634c4bdf654fd6e01c45a`，按其原始记录保留。中间审查提交 `15a4ac6` 的部分构建过程也保存在 `target/review-15a4ac6/`，后续修复由当前候选承接。

候选位于本机 `target/release-candidate-a1b1b02-macos15/`，未作开发者身份签名、公证或发布，不随 Git 提交。macOS 二进制从源码提交的独立导出目录重新构建，Linux 二进制复用下方历史候选中已验证的相同文件；包内 `BUILD-INFO.json` 声明相同源码提交与各二进制哈希。该声明不等同独立证明的构建来源或完全可复现构建。

| 目标 | 构建与运行环境 | 已观察证据 | 待补充 |
| --- | --- | --- | --- |
| Linux aarch64 | Ubuntu 22.04 用户空间、glibc 2.35；ARM 虚拟机 | release 构建，最高 GLIBC 符号版本 2.34；离线工具往返与使用流程 smoke 通过 | 更广泛的 Linux 环境 |
| Linux x86_64 | Ubuntu 22.04 用户空间、glibc 2.35；ARM 虚拟机中 Rosetta 模拟 | release 构建，最高 GLIBC 符号版本 2.34；离线工具往返与使用流程 smoke 通过 | 本候选未在原生 Intel 主机运行 |
| macOS aarch64 | macOS 27.2 ARM，Xcode 27.1、SDK 27.0 | release 构建及解包后二进制 smoke 通过；Mach-O `minos 15.0` | **尚未在 macOS 15 运行本候选** |
| macOS x86_64 | macOS 27.2 ARM 上交叉构建，Rosetta 执行 | release 构建及解包后二进制 smoke 通过；Mach-O `minos 15.0` | **尚未在 macOS 15 或原生 Intel 主机运行本候选** |

Linux 容器复用虚拟机内核，GLIBC 符号检查不代表所有 Linux 内核及运行时兼容性。macOS 的部署目标声明和 macOS 27 运行结果不能代替 macOS 15 的实际候选运行证据。

四平台候选经过离线 PLAN 文件读取／回传与工作区不变检查，以及配置、人工批准、退出、会话列表、恢复和继续输入的流程演练；测试使用临时工作区和本地模型替身，两个 x86_64 目标使用模拟。包校验和、成员与包内二进制哈希均已核对。该历史候选的 `SHA256SUMS`：

```text
2a02cc5d6bf14340477b41bc7c0dc995435d852e1523af61db6904721a58b25e  saya-0.1.0-linux-x86_64.tar.gz
8de8927b6736fc201ce3bf60a317370c7159086cbf9da82623b2da63e1fa1540  saya-0.1.0-linux-aarch64.tar.gz
77bb195af8faaca3a99656725765924cbf3d3cca6cf424f357e1f20cce1f3962  saya-0.1.0-macos-x86_64.tar.gz
b0f0fa786d4cf3c4263d624207818ee240a1aac46ececd7fcb95f2e826ad9489  saya-0.1.0-macos-aarch64.tar.gz
```

构建使用 Rust 1.99.1；macOS 显式设置 `MACOSX_DEPLOYMENT_TARGET=15.0`。Linux 使用 Ubuntu 22.04 与 [Dockerfile](../scripts/release/ubuntu-22.04.Dockerfile)；APT 软件包及 rustup 安装器未按内容固定。打包由 [package.py](../scripts/release/package.py) 完成；在候选目录可用 `shasum -a 256 -c SHA256SUMS` 复核。


## 历史候选：macOS 部署目标 13.0

D-035 前的候选仍保留在 `target/release-candidate-a1b1b02/`，macOS 二进制在 macOS 27.2 ARM（Intel 使用 Rosetta）完成 smoke，Mach-O `minos 13.0`；没有在 macOS 13 运行。其 macOS 包 SHA-256 分别为 x86_64 `c76e3e2b663559eaf7b30469367de6e2fa7ac70dde8159e4b754eb7a9a33555b`、aarch64 `f7ed458ecd3d3549d7a072a98907bb8f262f4d69b0813485d78897490407d816`。旧候选与旧最低版本验证要求不再作为当前发行目标，但保留作为历史证据。
