# 命令结构分析

DEV-075使用固定Tree-sitter0.27.1及Bash grammar0.25.1生成有界风险事实。入口为 `command_analysis::analyze`，正常exec授权通过同一次prepare取得规范化argv、cwd、完整环境摘要和实际过滤后的环境名称。

## 分析内容

直接argv保持字面参数。显式bash／sh／dash入口按选项定位脚本文本，再用真实CST提取命令、包装器、序列、管道、条件、重定向、后台、子shell及替换。env、timeout、command和exec包装器记录结构与不确定性。外部脚本、未知方言、动态展开、函数遮蔽、解释器代码及环境影响保留对应的unknown事实。

输出带schema版本、分析器与grammar版本、输入摘要、完成状态、原因、源码字节跨度及确定性工作预算。环境值由既有执行摘要绑定，分析器接收变量名称。赋值事实保留名称，shell源码以身份和跨度关联，避免在事实中重复展开。

## 预算

| 项目 | 默认上限 |
| --- | --- |
| 全部输入 | 64KiB／1024个参数 |
| 累计脚本文本 | 16KiB |
| CST遍历 | 8192节点／64层 |
| 包装器展开 | 8层 |
| 事实 | 512项／1MiB保留文本 |
| Parser progress callbacks | 8192次 |
| 协作deadline | 50ms |

错误语法、取消或预算耗尽返回Incomplete及原因，并保留可用的部分事实。deadline受宿主调度影响，Tree-sitter原生分配器的总内存硬限额由执行隔离能力另行表达。

## 审核与批准

`Operation::review_arguments`将宿主事实写入独立核查请求的 `__saya_command_facts`。这部分作为数据传入，固定英文审核提示词说明结构完整性与执行安全的区别。事实字段由宿主覆盖，正常ExecArgs继续拒绝未知参数字段。

已知凭据检查在完整审核数据构造后执行，包括CST字面拼接可能重构的内容。命中时转人工兜底，模型不收到该审核数据。

批准绑定规范化argv、cwd、完整环境摘要，以及 `__saya_command_analysis` 中的schema版本、分析器版本和限额。解析状态、进度和deadline耗时进入审核数据。实际派发继续使用一次性许可、当前工作模式、权限版本与后端约束。

静态事实用于判断可见风险。脚本文件、动态加载和宿主程序的实际行为仍需结合声明的任务、人工确认及运行期隔离处理。[Tree-sitter解析接口](https://docs.rs/tree-sitter/0.27.1/tree_sitter/struct.Parser.html)
