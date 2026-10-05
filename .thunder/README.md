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
├── plugins/                         # 既有：全局 TypeScript 插件
├── skills/                          # 既有：全局技能
├── scratchpad/  bridge/  conversations/   # 运行时产物

<workspace>/.thunder/                # 项目级（可提交）
├── models.json                      # provider 项目覆盖（已有）
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
├── plugins/                         # 项目 TypeScript 插件
└── tmp/                             # 写事务影子目录（运行时，gitignore）
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
    }
  }
}
```

`agent.permission` **刻意不支持**：能力档位由宿主的审批策略
（`SessionPolicy`）持有，让配置文件直接放宽会绕过该判定。

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
