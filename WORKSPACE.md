# Thunder 工作区说明

本文说明当前仓库怎么挂在 GitHub 上、目录怎么排、crate 之间怎么依赖，以及日常怎么开发。

## 1. 仓库怎么挂

GitHub 上**只有一个仓库**：

https://github.com/lucacicii/thunder

这是原来的 `thunder-agent-loop` 改名而来（旧 URL 会 301 过来），没有新建第二个仓库，也没有用 git submodule。

做法是把原来的 loop 仓库 **收成 monorepo**：

- git 根目录就是本工作区（`Documents/GitHub/thunder`）
- remote 是 `origin → lucacicii/thunder`
- 原来的 loop 源码从仓库根挪进了 `thunder-agent-loop/`
- 同级的 `core` / `root` / `providers` / `skills` / `mcp` / `plugin` 一并纳入这个仓库

克隆：

```bash
git clone https://github.com/lucacicii/thunder.git
cd thunder
```

提交、推送都在工作区根目录做，不要在子目录里再 `git init`。

## 2. 目录

```
thunder/                          # git 根 = GitHub 上的 lucacicii/thunder
├── README.md                     # 工作区入口
├── WORKSPACE.md                  # 本说明
├── run.sh                        # 启动 TUI
├── daemon.sh                     # 启动 STDIO Sidecar Daemon (供 Electron 集成)
├── test.sh                       # 全工作区测试
├── thunder-agent-loop/           # Agent A：单 Agent 闭环引擎
├── thunder-pi-bridge/            # pi-ai Node Sidecar Transport（统一模型方言与传输）
├── thunder-agent-providers/      # LLM Catalog 与配置解析
├── thunder-agent-skills/         # Skill 解析与注册
├── thunder-agent-mcp/            # MCP client / tool bridge
├── thunder-agent-root/           # 微内核 host，按任务动态挂插件
├── thunder-agent-daemon/         # STDIO Sidecar 守护进程（供 Electron / 外部前端集成）
├── thunder-agent-core/           # conversation / orchestra / TUI
│   ├── conversation/             # 会话与 Turn 存储
│   ├── thunder-orchestra/        # Agent B：多 Agent 调度
│   └── tui/                      # 交互终端
└── thunder-agent-plugin/         # 插件目录（占位）
```

各 crate 自己的 README 写模块细节。loop 的分层约定见 [`thunder-agent-loop/ARCHITECTURE.md`](thunder-agent-loop/ARCHITECTURE.md)。

## 3. 依赖关系

Cargo 全部用 **path 依赖**，不发 crates.io，也不引用 GitHub git URL。

```
thunder-agent-loop          # 底座，不依赖其它 Thunder crate
        ▲
        │
thunder-agent-providers / skills / mcp / conversation / orchestra
        ▲
        │
thunder-agent-root          # host：按需挂 conversation / orchestra / skills / mcp
        ▲
   ┌────┴────┐
   │         │
thunder-tui  thunder-agent-daemon  # 终端应用 / STDIO 守护进程
```

约定：

- **A = loop**：一个 `AgentLoop` 一次只跑一个任务；不感知调度器、会话、插件。
- **B = orchestra**：调度多个 A，不把 A 拆成外部 stepper。
- **host = root**：用插件把会话、编排、skills、MCP 接到 A 上。
- LLM 的 endpoint / api key **不放在** `AgentConfig` 里，由 host / providers 构造 `LLMClient` 再 `with_custom_client` 注入。未注入时 loop 使用 `UnconfiguredLLMClient`。

## 4. 运行与测试

需要 Rust 1.80+。

```bash
./run.sh          # 启动 thunder-tui
./test.sh         # 依次测 loop / providers / skills / mcp / root / core
```

单 crate：

```bash
(cd thunder-agent-loop && cargo test)
(cd thunder-agent-core && cargo test --workspace)
```

`target/`、`.thunder/`、`.env*` 已在根 `.gitignore` 中，不要提交。

## 5. 开发约定

- **一个 git、一个 remote。** 不要给 `root` / `core` / `plugin` 再挂 GitHub，也不要用 submodule 嵌 loop。
- **改 loop 也在这个仓库里提交。** 历史还在；只是路径从仓库根变成了 `thunder-agent-loop/`。
- **不要把嵌套 `.git` 带进子目录。** 否则会变成未跟踪的嵌套仓库，父仓库推不上去。
- 本地未推过的独立 git（以前的 root / core / plugin）已经去掉，历史以本仓库为准。

## 6. 和旧布局的差异

| 以前 | 现在 |
| --- | --- |
| GitHub 上只有 loop 源码，文件在仓库根 | 同一仓库，loop 在 `thunder-agent-loop/` |
| 其它 crate 只在本机，有的甚至没有 git | 全部进同一个 GitHub 仓库 |
| 工作区 `thunder/` 本身不是 git | `thunder/` 就是 git 根 |

旧 clone 若仍指向「根目录就是 Cargo.toml」的 loop，需要重新 clone，或在仓库根 `git pull` 后按新路径引用。
