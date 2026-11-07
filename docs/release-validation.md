# 0.1.0 发行候选验证记录

**状态：待验证，尚非可发布版本。** 根据 D-035，最低支持版本为 macOS 15；新的 15.0 目标候选尚未在 macOS 15 上完成双架构实测。当前候选的源码提交为 `a1b1b02d9fcc789b3f1634c4bdf654fd6e01c45a`，不等同于本记录所在的后续文档提交。

## 当前候选：macOS 部署目标 15.0

候选位于本机 `target/release-candidate-a1b1b02-macos15/`，未经签名、公证或发布，不随 Git 提交。macOS 二进制从源码提交的独立导出目录重新构建，Linux 二进制复用下方历史候选中已验证的相同文件；包内 `BUILD-INFO.json` 声明相同源码提交与各二进制哈希。该声明不等同独立证明的构建来源或完全可复现构建。

| 目标 | 构建与运行环境 | 已观察证据 | 待补充 |
| --- | --- | --- | --- |
| Linux aarch64 | Ubuntu 22.04 用户空间、glibc 2.35；ARM 虚拟机 | release 构建，最高 GLIBC 符号版本 2.34；离线工具往返与使用流程 smoke 通过 | 更广泛的 Linux 环境 |
| Linux x86_64 | Ubuntu 22.04 用户空间、glibc 2.35；ARM 虚拟机中 Rosetta 模拟 | release 构建，最高 GLIBC 符号版本 2.34；离线工具往返与使用流程 smoke 通过 | 本候选未在原生 Intel 主机运行 |
| macOS aarch64 | macOS 27.2 ARM，Xcode 27.1、SDK 27.0 | release 构建及解包后二进制 smoke 通过；Mach-O `minos 15.0` | **尚未在 macOS 15 运行本候选** |
| macOS x86_64 | macOS 27.2 ARM 上交叉构建，Rosetta 执行 | release 构建及解包后二进制 smoke 通过；Mach-O `minos 15.0` | **尚未在 macOS 15 或原生 Intel 主机运行本候选** |

Linux 容器复用虚拟机内核，GLIBC 符号检查不代表所有 Linux 内核及运行时兼容性。macOS 的部署目标声明和 macOS 27 运行结果不能代替 macOS 15 的实际候选运行证据。

四平台候选经过离线 PLAN 文件读取／回传与工作区不变检查，以及配置、人工批准、退出、会话列表、恢复和继续输入的流程演练；测试使用临时工作区和本地模型替身，两个 x86_64 目标使用模拟。包校验和、成员与包内二进制哈希均已核对。当前候选的 `SHA256SUMS`：

```text
2a02cc5d6bf14340477b41bc7c0dc995435d852e1523af61db6904721a58b25e  saya-0.1.0-linux-x86_64.tar.gz
8de8927b6736fc201ce3bf60a317370c7159086cbf9da82623b2da63e1fa1540  saya-0.1.0-linux-aarch64.tar.gz
77bb195af8faaca3a99656725765924cbf3d3cca6cf424f357e1f20cce1f3962  saya-0.1.0-macos-x86_64.tar.gz
b0f0fa786d4cf3c4263d624207818ee240a1aac46ececd7fcb95f2e826ad9489  saya-0.1.0-macos-aarch64.tar.gz
```

构建使用 Rust 1.99.1；macOS 显式设置 `MACOSX_DEPLOYMENT_TARGET=15.0`。Linux 使用 Ubuntu 22.04 与 [Dockerfile](../scripts/release/ubuntu-22.04.Dockerfile)；APT 软件包及 rustup 安装器未按内容固定。打包由 [package.py](../scripts/release/package.py) 完成；在候选目录可用 `shasum -a 256 -c SHA256SUMS` 复核。

## 待完成的最低环境验证

P6 提交 `a58da26` 的 [GitHub Actions 运行 42369396145](https://github.com/Ariakage/saya-harness/actions/runs/42369396145) 已在 macOS 15 ARM／Intel 和 Ubuntu 22.04 ARM／Intel 通过，但它使用旧的 13.0 部署目标，也未运行当前候选包的验证，不能代替本候选验收。更新后的 [GitHub Actions 运行 42373322880](https://github.com/Ariakage/saya-harness/actions/runs/42373322880) 在提交 `47452e6` 上四平台全部通过：macOS 15 的两架构均通过 15.0 部署版本检查，四个目标均通过 [smoke.py](../scripts/release/smoke.py) 与 [journey.py](../scripts/release/journey.py)。CI 构建产物与此本地候选不同，不能把 CI 测试归因于本地包；该工作流不上传构建产物。

macOS 15 机器上可使用 Python 3 运行下列命令，路径替换为实际位置且输出文件必须不存在。脚本检查系统版本、包与二进制哈希、Mach-O 架构和部署目标，并对解包后的二进制运行两项离线演练：

```sh
python3 scripts/release/verify_macos15.py /path/to/saya-0.1.0-macos-aarch64.tar.gz --sha256 b0f0fa786d4cf3c4263d624207818ee240a1aac46ececd7fcb95f2e826ad9489 --output macos15-aarch64.json
python3 scripts/release/verify_macos15.py /path/to/saya-0.1.0-macos-x86_64.tar.gz --sha256 77bb195af8faaca3a99656725765924cbf3d3cca6cf424f357e1f20cce1f3962 --output macos15-x86_64.json
```

在 ARM 主机通过 Rosetta 验证 Intel 候选时，应记录模拟情况。脚本的模拟单元测试不构成 macOS 15 实际验收。完成双架构候选实测后，核对 B-01 至 B-24、AC-01 至 AC-24、最终版本说明及候选哈希，再判断 P7 能否结束；外部推送和发布须遵循当时的用户指令。

## 历史候选：macOS 部署目标 13.0

D-035 前的候选仍保留在 `target/release-candidate-a1b1b02/`，macOS 二进制在 macOS 27.2 ARM（Intel 使用 Rosetta）完成 smoke，Mach-O `minos 13.0`；没有在 macOS 13 运行。其 macOS 包 SHA-256 分别为 x86_64 `c76e3e2b663559eaf7b30469367de6e2fa7ac70dde8159e4b754eb7a9a33555b`、aarch64 `f7ed458ecd3d3549d7a072a98907bb8f262f4d69b0813485d78897490407d816`。旧候选与旧最低版本验证要求不再作为当前发行目标，但保留作为历史证据。
