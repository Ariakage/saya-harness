# 日志与追踪设计

**DEV-040 至 DEV-042 已实现五级类型化日志、受控 tracing 接入与 CLI 查询。实际验证记录见[开发进度](progress.md)。**

## 已有实现

[logging.rs](../crates/saya-core/src/logging.rs) 提供 `JsonLog`，接受 `trace`、`debug`、`info`、`warn`、`error`，默认严重程度阈值为 `warn`，在已选 state 目录写 JSON 行，单文件 10 MiB、最多五份备份，写入前进行已知凭据脱敏，并用跨进程锁保护轮转。CLI 的 `log_run_result` 记录未完成运行的摘要，`log_warning` 处理写入失败。

[events.rs](../crates/saya-cli/src/events.rs) 的事件输出与 SQLite 持久事件负责运行展示及恢复。新增诊断日志与这些数据分别维护，并通过 ID 关联。

## 类型化记录

`DiagnosticRecord` 使用 `LogContext` 关联 session／run／request／call／job／span，`LogFields` 白名单接受状态、错误码、profile、协议、目的、耗时、字节数、数量及丢弃数。新记录保存 `schema_version = 1`、UTC 时间、进程号和本 logger 的单调序号。旧 `record` API 与旧行解码继续可用。

每条记录最多 64 KiB，写入前使用统一脱敏处理已知凭据与嵌套 JSON。默认阈值在打开目录前过滤，配置为 trace 后可记录全部五级。日志文件为 0600，新建目录为 0700，拒绝符号链接、硬链接、其他用户拥有的文件及过宽文件权限。写锁最多等待一秒，失败通过 `log_error` 返回。轮转维持 10 MiB 与五份备份。

## tracing 写入与关联

`logging::diagnostics::Diagnostics` 管理本地 writer 线程和 subscriber，应用入口安装 dispatch 并保留 guard 至退出。默认普通队列容量 256，warn／error 优先队列容量 32，普通事件队列满后计数丢弃，高优先事件最多等待 20 毫秒。`flush` 返回写入失败或超时，退出最多等待两秒，`diagnostics.dropped` 记录累计变化。

subscriber 只接收 `saya`／`saya::` target，按类型化白名单采集字段，未知字段和 message 被过滤。超长单字段以 withheld 标记处理后再进入队列。span 保存 session／run／request／call／job 归属，后台任务显式传播 subscriber 与 span。运行、模型执行、独立审核、工具派发和后台状态通过同一诊断层关联，模型完成记录包含单调时钟耗时。

## 五级日志

| 级别 | 记录内容 |
| --- | --- |
| trace | 状态边界、调度阶段、队列与 span 元数据 |
| debug | 参数校验结果、连接阶段、限额判断及耗时 |
| info | 会话／运行／工具生命周期摘要 |
| warn | 可恢复错误、重试、权限拒绝及能力降级原因 |
| error | 运行失败、存储失败及需要处理的系统错误 |

默认级别为 `warn`，通过 `--log-level` 或 `SAYA_LOG_LEVEL` 显式选择其他级别，优先级为 CLI 高于环境变量、高于默认值。首批保留现有配置文件加载方式。

```sh
saya --log-level debug chat
saya --log-level trace run "Investigate the failure"
saya logs show --session SESSION_ID --level warn
saya --output jsonl logs tail --session SESSION_ID
saya --output json logs show --level info --limit 100 --offset 0
saya logs show --since 2026-11-08T08:00:00+08:00
```

级别过滤按严重程度阈值处理，`warn` 同时包含 `error`。时间范围使用带时区输入，保存的时间为 UTC。耗时使用单调时钟，日志排序同时参考进程、关联 ID 和本地序号。

## 日志查询

`logs show` 支持 `--session`、`--run`、`--level`、`--since`、`--until`、`--limit` 与 `--offset`，查询阈值默认 trace，包含所有已保存级别。每页默认 100 行、最多 1,000 行，offset 最大 1,000,000。时间使用显式时区的 RFC 3339，起止边界均包含。查询返回 `records`、`next_offset` 与 `malformed_lines`，兼容旧记录中 data 的 session／run 关联。

查询只读当前与五份轮转文件，缺失日志返回空页。每文件最多读取 10 MiB、每行最多 64 KiB，超长、损坏及末尾部分行有明确省略计数，并按当前配置重新脱敏。JSON 使用版本信封，JSONL 逐行输出 `log.entry` 和 `logs.page`。text 输出 JSON 转义文本，额外转换 DEL／C1 等控制字符。

`logs tail` 使用同一筛选与每批 limit，按 inode、持有的文件描述符及完整行 offset 跟随轮转，重复观察保持游标，部分行等待写完。扫描由 blocking worker 执行并检查共享取消，管道输出使用非阻塞小块写入，慢消费者时仍可响应 SIGINT／SIGTERM，退出码 130。tail 提供 text／JSONL 表达，选择 JSON 返回用法错误。

## 接入与记录结构

使用 `tracing` 的 span／event 作为进程内诊断入口，在可执行入口安装 subscriber，安全记录层负责字段选择、脱敏、大小检查和轮转。库层发出结构化诊断，应用入口管理订阅生命周期。异步任务通过显式 span 传播归属。设计参考官方 [tracing](https://docs.rs/tracing/latest/tracing/) 和 [tracing-subscriber](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/) 文档。

记录保留 `timestamp`、`level`、`event`、`data`，在兼容读取旧行的基础上扩展格式版本、target、trace／span、session／run／request／call／job ID、状态、耗时和字节计数。新增字段由 DEV-040 的类型化记录固定，旧日志按缺失元数据读取。

记录路径采用以下顺序。

1. 采集类型化字段，文本参数使用字段白名单。
2. 对结构化值执行已知凭据遮蔽，处理嵌套／编码内容及控制字符。
3. 执行单行与队列上限检查，附加关联元数据。
4. 通过现有私有目录、文件权限、跨进程锁和轮转机制保存。

Prompt、环境变量、HTTP 认证头、命令完整环境和工具正文具有独立的数据访问范围。五级日志默认记录其 ID、大小、耗时和状态。第三方诊断按 target 白名单接入，过滤发生在落盘与显示之前。

## 并发、失败与输出

- 使用有界后台写队列承接阻塞文件操作。队列满时记录诊断丢弃计数，保留 warn／error 的优先通道，并设置有界等待和退出排空策略。
- 关键业务事件继续通过持久化入口保存。日志写入失败显示经过脱敏的降级状态，涉及执行意图／结果的存储失败按现有运行终止契约处理。
- CLI 诊断与运行 JSON／JSONL 的 stdout 分离。TUI 中由界面展示诊断状态，终端绘制保持单一输出所有者。
- `logs show` 分页读取轮转文件，`tail` 跟随文件更换。二者处理损坏尾行、取消、符号链接和私有文件权限，输出经过终端安全转换。
- 日志保持本地保存，远端追踪接收端属于后续独立集成。

## 验收

DEV-040 验证全部级别、旧行读取、字段白名单、深层编码秘密、权限、符号链接和跨进程轮转。DEV-041 验证并发模型／核查／工具／后台任务的 span 归属，以及队列过载、慢磁盘与退出排空。DEV-042 验证筛选、跟随、取消和 JSON／JSONL stdout。

诊断完整性、业务事件完整性和副作用保存分别给出结果，测试使用本地模型、合成凭据及故障注入。任务依赖见[后续开发计划](post-mvp-plan.md)。
