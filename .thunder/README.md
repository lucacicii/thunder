# `.thunder/`

Thunder 的用户级与项目级配置目录。

- `~/.thunder/` —— 用户级，仓库之外，通常不提交
- `<workspace>/.thunder/` —— 项目级，可提交（本机覆盖用 `*.local.*`）

> 与 `.arp/` **无关**。`.arp/` 属于
> [`agent-resume-panel`](https://github.com/lucacicii/agent-resume-panel)（面板配置 + 共享产物）；
> Thunder 不读取它的任何内容。

---

## 目录布局

```text
~/.thunder/                         # 用户级
├── models.json                     # provider 目录
├── auth.json                       # provider 密钥（勿提交）
├── models_metadata.json            # 模型元数据缓存
├── config.json                     # agent 配置默认值
├── mcp.json                        # 用户级 MCP servers
├── THUNDER.md                      # 用户级长期记忆
├── permissions.json                # 记住的「always allow」规则
├── skills/                         # 全局技能
├── prompts/                        # 用户级提示模板
├── logs/                           # 日志（如 tui.log）
├── scratchpad/                     # 运行时草稿
└── conversations/                  # 会话持久化

<workspace>/.thunder/               # 项目级（可提交）
├── config.json                     # 项目 agent 配置（覆盖用户级）
├── config.local.json               # 本机覆盖（gitignore）
├── mcp.json                        # 项目 MCP servers
├── THUNDER.md                      # 项目长期记忆
├── THUNDER.local.md                # 本机记忆（gitignore）
├── memory/*.md                     # 分片记忆
├── prompts/                        # 项目提示模板
└── plans/                          # plan 模式产出的计划（运行时，gitignore）
```

**优先级**（后者覆盖前者）：内置默认 → `~/.thunder` → `<ws>/.thunder` → `<ws>/.thunder/*.local.*`。

用户级数据根可整体重定向：环境变量 `THUNDER_CONFIG_DIR`，或二进制参数 `--config-dir <path>`。
它管的是**全部**用户级状态（上面那一列），所以宿主（Electron 等）能把一个实例完全收进自己的目录。
解析逻辑集中在 `thunder-agent-loop/src/core/paths.rs`；`~/.agents`、`~/.pi`、shell rc 这类跨工具路径不受影响，仍取真实 HOME。

---

## `config.json`

Schema 版本 `1`。未声明的分组/字段表示"未配置"，不是空覆盖。

```jsonc
{
  "version": 1,
  "agent": {
    // 追加到系统提示末尾（不替换内置提示）。相对 <ws>/.thunder/。
    "systemPromptFile": "prompts/system.md",
    // 自主循环轮数上限。
    "maxTurns": 50,
    // 默认思考档位。
    "thinkingLevel": "high",
    "memory": {
      "enabled": true,
      // 额外记忆文件，相对 <ws>/.thunder/。追加在 THUNDER.md / memory/*.md 之后。
      "files": ["extra/notes.md"]
    },
    // 新会话的初始审批模式：plan | ask | acceptEdits | yolo（旧名也认）。
    // 字段注释写的是：None = yolo。宿主的 Mode::default() 是 Ask——
    // 前者是"配置没写时走哪档"，后者是"代码里没配置时走哪档"，别混。
    "defaultMode": "ask"
  },
  "plugins": {
    // 显式启用的内建工具；未配置或留空 = 全部启用。
    "builtin": ["read_file", "write_file", "bash", "grep", "find", "ls"],
    // 禁用列表：组件 id（memory / skills / web / mcp / conversation / metrics /
    // agent_status / mode）或工具名（含别名）。
    "disabled": ["bash"],
    // 声明式外部 MCP servers；与 mcp.json 等价，合并时以声明式为准。
    "mcpServers": {
      "github": {
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-github"],
        "env": { "GITHUB_TOKEN": "..." },
        "timeout_ms": 120000,
        "max_frame_bytes": 4194304
      }
    }
  },
  "web": {
    "search": { "provider": "auto" },   // free | auto | deepseek；默认 free
    "fetch":  { "provider": "auto" },   // local | auto | deepseek；默认 local
    "deepseek": {
      "apiKey": "${DEEPSEEK_API_KEY}",
      "baseUrl": "https://api.deepseek.com",
      "headers": {},
      "timeoutMs": 15000,
      "retries": 1
    }
  }
}
```

### `plugins.disabled` 的语义

0.6 起没有"插件选择器"：宿主注册了什么，这次运行就有什么。`disabled` 是**唯一的减法**，在
`Assembly::without_disabled` 里一次性应用，匹配的是 section / pack / observer 的 `id()`
（`memory` / `skills` / `web` / `mcp` / `conversation` / `metrics` / `agent_status` / `mode`）
以及注册表里的工具名（支持 `read` ⇄ `read_file` 这类别名）。

### `agent.defaultMode` 与代码默认值

- 这个字段**当前只有 daemon 消费**（`service.rs` 在每次 task 开始时读它）；`ThunderRoot` 本身不读，TUI 也不读——两者的起点都是 `Mode::default()` = `Ask`。
- 所以：daemon 会话的初始档位可以来自配置；TUI 的初始档位来自代码默认值或 `/mode`。
- `yolo` 必须显式写出来。它意味着"不再询问"，守卫仍然生效。

### 网页工具后端

| `provider` | `web_search` | `web_fetch` |
|---|---|---|
| `free`（搜索默认） | 免 key 的免费引擎 | — |
| `local`（抓取默认） | — | 本地抓取 + 转换，不产生费用 |
| `deepseek` | DeepSeek Search API | DeepSeek Fetch API（按 token 计费） |
| `auto` | 有 DeepSeek 凭据就用，否则回退 | 同左 |

DeepSeek 凭据的解析顺序：`web.deepseek.apiKey` → `auth.json` 的 `deepseek` 条目 →
`DEEPSEEK_API_KEY` → `models.json` 里指向 `api.deepseek.com` 的模型。
中转（relay）的 key 不会被借用，`THUNDER_API_KEY` 也不会。

---

## 长期记忆

注入顺序（低 → 高）：

1. `~/.thunder/THUNDER.md`
2. `<ws>/.thunder/THUNDER.md`
3. `<ws>/.thunder/memory/*.md`（按文件名排序）
4. `<ws>/.thunder/THUNDER.local.md`
5. `<ws>/.thunder/<agent.memory.files[]>`

- 文件里的 `@path/to/file.md` 会递归展开：相对当前文件、限制在工作区与数据根内、最大深度 5、总量 256 KB。
- 记忆块在**每次运行准备阶段**渲染一次并缓存，位于请求前缀——这是 provider 侧 prompt cache 能稳定的前提。
- 写入走 `memory_write` 工具，目标被 jail 在 `<ws>/.thunder/` 内（拒绝绝对路径与 `..`，只允许 `.md`）。

---

## `permissions.json`

「always allow」记住的规则。

```jsonc
{
  "version": 1,
  "rules": [
    { "tool": "bash", "arg_prefix": "git status" },
    { "tool": "write_file", "arg_prefix": "/ws/src/lib.rs" }
  ]
}
```

- 位置：`<数据根>/permissions.json`（默认 `~/.thunder/permissions.json`），**用户级、跨工作区**。
- 规则只增不减；损坏、版本不符或读不到 = 空列表（这个文件只会授予，读失败就是什么都不授予）。
- 写入是临时文件 + rename（原子）。
- plan 模式下不查询规则，也不提供 "always allow" 选项。
- 匹配细节见 [`docs/permission-structure.md`](../docs/permission-structure.md) §5。

---

## `plans/`

plan 模式的产物：`<ws>/.thunder/plans/<sanitized-session>.md`。

- `write_plan` 原子覆盖写入；
- `exit_plan_mode` 读取并提交审批；
- 会话 id 会被消毒成安全文件名（这是唯一的防线）；
- 建议 gitignore。
