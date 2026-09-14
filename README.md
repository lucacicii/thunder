# Thunder

Thunder Agent 工作区。代码托管在现有仓库 [lucacicii/thunder-agent-loop](https://github.com/lucacicii/thunder-agent-loop)（未新建 GitHub 仓库）。

## Crates

| 目录 | 说明 |
| --- | --- |
| `thunder-agent-loop` | Agent Loop 引擎 |
| `thunder-agent-providers` | LLM Provider 适配 |
| `thunder-agent-skills` | Skill 解析与注册 |
| `thunder-agent-mcp` | MCP client / tool bridge |
| `thunder-agent-root` | 微内核 host 与插件编排 |
| `thunder-agent-core` | conversation / orchestra / TUI |
| `thunder-agent-plugin` | 插件目录 |

## 命令

```bash
./run.sh      # 启动 TUI
./test.sh     # 全工作区测试
```
