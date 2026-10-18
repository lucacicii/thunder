# `.thunder/`

Thunder 自身的用户级与项目级配置目录。

- `~/.thunder/` — 用户级（仓库之外，不受版本控制）
- `<workspace>/.thunder/` — 项目级（可提交，随仓库分发）

> 与 `.arp/` **无关**。`.arp/` 属于
> [`agent-resume-panel`](https://github.com/lucacicii/agent-resume-panel)（面板配置 +
> 共享产物）。Thunder 不读取 `.arp/` 下的任何配置。

---

## 目录布局

```
~/.thunder/                          # 用户级
├── models.json                      # provider 目录（已有）
├── auth.json                        # provider 密钥（已有，勿提交）
├── config.json                      # ★ agent 配置默认值
├── mcp.json                         # ★ 用户级 MCP servers
├── THUNDER.md                       # ★ 用户级长期记忆
├── prompts/                         # ★ 用户级提示模板
│   └── system.md                    #    系统提示追加段
├── plugins/                         # 遗留目录（0.4 起无消费者，可删）
├── skills/                          # 既有：全局技能
├── logs/                            # TUI 日志文件（tui.log）
├── scratchpad/  conversations/      # 运行时产物
├── permissions.json                 # 同意过的 always allow 规则（daemon / TUI 写）

<workspace>/.thunder/                # 项目级（可提交）
├── models.json                      # provider 项目覆盖（读时**不**覆盖用户级，见下）
├── auth.json                        # 项目密钥（gitignore）
├── config.json                      # ★ 项目 agent 配置（覆盖用户级）
├── config.local.json                # ★ 本机覆盖（gitignore）
├── mcp.json                         # ★ 项目 MCP servers
├── THUNDER.md                       # ★ 项目长期记忆
├── THUNDER.local.md                 # ★ 本机记忆（gitignore）
├── memory/                          # ★ 分片记忆
│   └── *.md
├── prompts/                         # ★ 提示模板 / 自定义 slash
│   └── *.md
├── plugins/                         # 遗留目录（0.4 起无消费者，可删）
└── plans/                           # plan 模式产出的计划文件（运行时，gitignore）
```

**优先级**（后者覆盖前者）：内置默认 → `~/.thunder` → `<ws>/.thunder` →
`<ws>/.thunder/*.local.*`。

---

## `config.json`

Schema 版本 `1`。**未声明的分组/字段表示"未配置"，不是空覆盖**。

```jsonc
{
  "version": 1,
  "agent": {
    // 追加到系统提示末尾（不替换内置提示）。相对 `<ws>/.thunder/`。
    "systemPromptFile": "prompts/system.md",
    // 自主循环轮数上限。
    "maxTurns": 50,
    // 默认思考档位。
    "thinkingLevel": "high",
    "memory": {
      "enabled": true,
      // 额外记忆文件，相对 `<ws>/.thunder/`。
      "files": ["extra/notes.md"]
    },
    // 新会话默认落在哪个审批模式。
    // plan | ask | acceptEdits | yolo（旧名亦可）；未设 = yolo。
    "defaultMode": "ask"
  },
  "plugins": {
    // 显式启用的内建工具插件（未配置或留空则默认全量启用）
    "builtin": ["read_file", "write_file", "bash", "grep", "find_by_name", "list_dir"],
    // 禁用的插件或工具（支持别名，如 "bash", "read", "write", "ls"）
    "disabled": ["bash"],
    // 声明式挂载的外部 MCP 服务
    "mcpServers": {
      "github": {
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-github"],
        "env": { "GITHUB_TOKEN": "..." }
      }
    }
  },
  // 网页工具后端（web_fetch / web_search）。整组可省略。
  "web": {
    "search": {
      // free | auto | deepseek；未设 = free（免 key 引擎，不产生费用）
      "provider": "auto"
    },
    "fetch": {
      // local | auto | deepseek；未设 = local（本地抓取，不产生费用）
      "provider": "auto"
    },
    "deepseek": {
      // 未设时依次回退：models.json 里指向 api.deepseek.com 的 deepseek
      // 模型 → auth.json 的 "deepseek" 条目 → DEEPSEEK_API_KEY。
      // 中转（relay）的 key 不会被借用，THUNDER_API_KEY 也不会。
      "apiKey": "${DEEPSEEK_API_KEY}",
      // 未设 = https://api.deepseek.com（末尾的 /v1 会自动去掉）。
      "baseUrl": "https://api.deepseek.com",
      // 额外请求头（中转/代理需要路由头时用）。
      "headers": {},
      // 单次请求超时；未设 = search 10s / fetch 15s。
      "timeoutMs": 15000,
      // 408 / 429 / 5xx 的重试次数；未设 = 1。
      "retries": 1
    }
  }
}
```

### 网页工具后端

| `provider` | `web_search` | `web_fetch` |
| --- | --- | --- |
| `free` / `local` | DuckDuckGo → Exa | 本地抓取（跨域重定向回传给模型） |
| `auto` | 有 key 时 DeepSeek → DuckDuckGo → Exa | 本地抓取 → 失败或 403/408/429/5xx 时 DeepSeek Fetch |
| `deepseek` | 仅 DeepSeek；无 key 直接报错 | DeepSeek Fetch → 失败时本地抓取 |

- **默认不产生费用**：只有配置里写了 `auto` / `deepseek`，且能解析到 key 时才会调用 DeepSeek。
- DeepSeek Search / Fetch 按**成功请求**计费（5 元 / 1000 次），Search 无结果不计费；账号级限速 5 QPS。
- DeepSeek 搜索会把本轮用户输入作为 `ori_question` 一并发送，帮助索引理解意图。
- 本地抓取判定为**跨域重定向**时不降级到 DeepSeek：那条路径要求模型自己决定是否跟随。
- `auto` 下 404 不触发 DeepSeek：页面确实不存在，付费再问一次没有意义。
- `web_search` 的 `recency` 在 DeepSeek 后端会翻译成 `after:YYYY-MM-DD` 操作符。

**没有 `agent.permission`**：能力档位（tier）在 0.4 已删除，工具始终全量注册。
能不能跑由 `agent.defaultMode` 选的模式决定，模式本身住在宿主可卸载的
`ModePlugin` 里——配置文件只能选一个**已有**的模式，不能发明放宽规则。
若需禁用特定工具或接入外部 MCP 服务，请使用 `plugins.disabled` 与 `plugins.mcpServers`。

`plans/` 目录由 plan 模式使用：计划文件写到
`<ws>/.thunder/plans/<session>.md`，文件名里的 session 会被消毒成
`[A-Za-z0-9._-]`。

---

## 长期记忆（Memory）

按以下顺序加载 Markdown，全部**追加**进系统提示：

| 顺序 | 来源 |
| --- | --- |
| 1 | `~/.thunder/THUNDER.md` |
| 2 | `<ws>/.thunder/THUNDER.md` |
| 3 | `<ws>/.thunder/memory/*.md`（按文件名排序） |
| 4 | `<ws>/.thunder/THUNDER.local.md` |
| 5 | `config.json` 的 `agent.memory.files[]` |

任一文件可用 `@path/to/file.md` 导入另一文件：

- 相对路径按**导入文件所在目录**解析；
- 越狱防护：导入路径必须落在工作区或 `~/.thunder` 内；
- 深度上限 `5` 层，总大小上限 `256 KB`，重复/循环导入被去重。

### 提示缓存稳定性

记忆块位于每次请求的 **position 0**，正是 provider 提示缓存所键控的区域。
因此记忆文件**每次 host 初始化只读取一次**并缓存；编辑在下一次会话
（或 host 重建 run）时生效，避免逐轮抖动导致前缀缓存失效。

---

## MCP 配置

`mcp.json` 采用标准 `mcpServers` schema，与 `mcp_servers.json` / `.mcp.json`
兼容。项目级 `.thunder/mcp.json` **优先**于工作区根的通用文件：

```jsonc
{
  "mcpServers": {
    "notes": { "command": "npx", "args": ["-y", "@acme/notes-mcp"] }
  }
}
```

查找顺序（`McpConfig::find_and_load_from_workspace`）：
`<ws>/.thunder/mcp.json` → `<ws>/mcp_servers.json` → `<ws>/.mcp.json` →
用户级 `~/.thunder/mcp.json` → `~/.mcp.json` → …（Cursor / Claude 兼容位）。
