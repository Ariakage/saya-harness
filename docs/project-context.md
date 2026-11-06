# 项目指令与 Skill 发现

依据 [D-033](decisions.md)，CLI 运行时默认发现以下约定来源，按列出顺序提供上下文；不存在的来源会跳过：

1. 用户主目录 `~/.agents/AGENTS.md`、`~/.saya/AGENTS.md`。
2. 工作区根目录 `AGENTS.md`、`.agents/AGENTS.md`、`.saya/AGENTS.md`。
3. 目标路径从工作区根到目标目录之间存在的 `AGENTS.md`，由外到内排列。

用户级规则先于工作区规则，更具体的工作区规则适用对应路径；冲突时工作区优先。内容编码为 `saya_project_context_v1` 数据项，通过模型历史的 user 消息传递，而非系统指令；提示词要求模型把它视作上下文，不能把该要求视作提示注入的绝对防护。运行时独立执行权限、工作模式和工具策略，项目内容不能改变这些控制，也不能修改运行配置或授权。选定的配置文件与 `SAYA_*` 优先级仍按 [B-04](mvp-baseline.md) 处理，不自动合并 `.saya` 内的运行配置。

同时发现 `~/.agents/skills/*/SKILL.md`、`~/.saya/skills/*/SKILL.md`、工作区内 `.agents/skills/*/SKILL.md` 与 `.saya/skills/*/SKILL.md` 的名称、描述、来源路径和 SHA-256。支持 [Agent Skills 格式](https://agentskills.io/specification)中的 YAML frontmatter；同名 Skill 以工作区优先，同一作用域 `.saya` 优先于 `.agents`。无效名称、缺失或无效元数据会跳过；为限制解析风险，包含 YAML anchor／alias 的 frontmatter 不参与发现。`allowed-tools` 不授予任何工具权限。

发现仅将元数据发给模型，不自动注入 Skill 正文、安装插件或运行脚本。需要正文时仍通过受权限控制的文件读取工具；用户级 Skill 位于默认工作区范围之外，需用户将其目录配置为 `permissions.additional_roots` 或选择适用的权限设置才能由工具读取。完整 Skill 激活、资源管理与插件生命周期仍属后续能力。

每次模型调用前重新读取来源并计算指纹；已生成的工具调用在派发前重查其目标路径的作用域，若发现适用内容发生变化，则不执行该调用，先将更新后的上下文提供给模型。人工批准后和后台实际派发前也会重查。无法安全读取来源、路径逃逸或超出大小限制时报错，不静默将其当作没有指令；Skill 目录条目不能通过链接逃逸发现根目录。文件系统可能在检查后再次被外部进程改变，这不是原子快照或操作系统沙箱。

当前来源和指纹仅在运行内存中使用，未持久化；会话恢复与来源记录仍待 P5。完整交互会话仍待 DEV-020，现有 CLI 能力见[开发预览](cli-preview.md)。
