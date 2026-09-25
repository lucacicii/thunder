# Thunder

Thunder Agent 工作区。托管在 [lucacicii/thunder](https://github.com/lucacicii/thunder)（由原来的 `thunder-agent-loop` 改名而来），没有新建 GitHub 仓库，也没有 submodule。

仓库怎么挂、目录怎么排、crate 怎么依赖，见 **[WORKSPACE.md](WORKSPACE.md)**。

## Crates

| 目录 | 说明 |
| --- | --- |
| [`thunder-agent-loop`](thunder-agent-loop) | Agent Loop 引擎（单 Agent 闭环） |
| [`thunder-pi-bridge`](thunder-pi-bridge) | pi-ai Node Sidecar Transport（统一模型方言与传输） |
| [`thunder-agent-providers`](thunder-agent-providers) | LLM Catalog 与配置解析 |
| [`thunder-agent-skills`](thunder-agent-skills) | Skill 解析与注册 |
| [`thunder-agent-mcp`](thunder-agent-mcp) | MCP client / tool bridge |
| [`thunder-agent-root`](thunder-agent-root) | 微内核 host 与插件编排 |
| [`thunder-agent-daemon`](thunder-agent-daemon) | STDIO Sidecar 守护进程（供 Electron / 外部前端集成） |
| [`thunder-agent-core`](thunder-agent-core) | conversation / orchestra / TUI |
| [`thunder-agent-plugin`](thunder-agent-plugin) | 插件目录 |

## 命令

```bash
git clone https://github.com/lucacicii/thunder.git
cd thunder
./run.sh      # 启动 TUI
./daemon.sh   # 启动 STDIO Sidecar 守护进程（供 Electron / 外部前端集成）
./test.sh     # 全工作区测试
```
