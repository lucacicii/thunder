# Thunder

Thunder Agent 工作区。托管在现有仓库 [lucacicii/thunder-agent-loop](https://github.com/lucacicii/thunder-agent-loop)，没有新建 GitHub 仓库，也没有 submodule。

仓库怎么挂、目录怎么排、crate 怎么依赖，见 **[WORKSPACE.md](WORKSPACE.md)**。

## Crates

| 目录 | 说明 |
| --- | --- |
| [`thunder-agent-loop`](thunder-agent-loop) | Agent Loop 引擎（单 Agent 闭环） |
| [`thunder-agent-providers`](thunder-agent-providers) | LLM Provider 适配 |
| [`thunder-agent-skills`](thunder-agent-skills) | Skill 解析与注册 |
| [`thunder-agent-mcp`](thunder-agent-mcp) | MCP client / tool bridge |
| [`thunder-agent-root`](thunder-agent-root) | 微内核 host 与插件编排 |
| [`thunder-agent-core`](thunder-agent-core) | conversation / orchestra / TUI |
| [`thunder-agent-plugin`](thunder-agent-plugin) | 插件目录 |

## 命令

```bash
git clone https://github.com/lucacicii/thunder-agent-loop.git thunder
cd thunder
./run.sh      # 启动 TUI
./test.sh     # 全工作区测试
```
