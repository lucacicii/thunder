# Thunder 工作区架构说明

[English](WORKSPACE_en.md) | [简体中文](WORKSPACE.md)

本文档阐述当前 Thunder 仓库的代码组织拓扑、各 Crate 的依赖关系以及日常开发与测试规范。

---

## 1. 统一代码仓库

GitHub 仓库地址：

https://github.com/lucacicii/thunder

全生态统一组织为 **单一 Git Monorepo** 与 **统一 Cargo Workspace**（根目录单个 `Cargo.toml` 治理 10 个 Crate 与单个 `Cargo.lock`），杜绝嵌套仓库或 git submodule。

- git 根目录即本工作区根目录
- remote 指向 `origin → lucacicii/thunder`
- 所有提交、分支、标签与推送均在工作区根目录执行，切勿在子目录中重复执行 `git init`

克隆仓库：

```bash
git clone https://github.com/lucacicii/thunder.git
cd thunder
```

---

## 2. 目录布局

```text
thunder/                          # Git Monorepo 根目录与 Cargo Workspace 根
├── README.md                     # 中文主页说明
├── README_en.md                  # 英文主页说明
├── WORKSPACE.md                  # 中文工作区规范
├── WORKSPACE_en.md               # 英文工作区规范
├── Cargo.toml                    # 统一工作区配置（10 个成员包）
├── Cargo.lock                    # 统一共享锁文件
├── run.sh                        # 启动 TUI 终端
├── daemon.sh                     # 启动 STDIO Sidecar 守护进程
├── test.sh                       # 全工作区测试套件脚本
├── thunder-agent-loop/           # Agent A：单 Agent 闭环执行引擎
├── thunder-pi-bridge/            # pi-ai Node Sidecar 传输底座（打包 @earendil-works/pi-ai）
├── thunder-agent-providers/      # LLM Catalog 与配置解析
├── thunder-agent-skills/         # Skill 解析、注册与全局缓存
├── thunder-agent-plugin/         # TypeScript 单文件插件宿主引擎
├── thunder-agent-mcp/            # MCP 客户端与动态工具桥接
├── thunder-agent-root/           # 微内核 host，按任务动态挂载插件
├── thunder-agent-daemon/         # STDIO Sidecar 守护进程（供 Electron / 外部前端集成）
└── thunder-agent-core/           # 核心业务组件集合
    ├── conversation/             # 会话与 Turn 存储（thunder-conversation）
    └── tui/                      # 终端界面（thunder-tui）
```

---

## 3. 依赖关系与架构分层

全工作区所有包之间均使用 **Path 依赖**，严禁使用 git URL 交叉引用。

```text
                       ┌─────────────────────────┐
                       │    thunder-pi-bridge    │ (Node Sidecar: pi-ai)
                       └────────────┬────────────┘
                                    │ implements LLMClientTrait
                                    ▼
┌────────────────────┐   ┌───────────────────────────┐
│ thunder-agent-loop │ ◄─┤  thunder-agent-providers  │
└─────────┬──────────┘   └─────────────┬─────────────┘
          │ (Kernel: Unit)             │
          ▼                            │
┌──────────────────────────────────────┴─────────────────────────────────┐
│ 插件/组件生态: skills / plugin / mcp / conversation                          │
└──────────────────────────────────────┬─────────────────────────────────┘
                                       │
                                       ▼
                             ┌────────────────────┐
                             │ thunder-agent-root │ (Host: Microkernel)
                             └─────────┬──────────┘
                                       │
                      ┌────────────────┴────────────────┐
                      ▼                                 ▼
              ┌───────────────┐               ┌──────────────────────┐
              │  thunder-tui  │               │ thunder-agent-daemon │
              │ (Terminal UI) │               │   (STDIO Sidecar)    │
              └───────────────┘               └──────────────────────┘
```

### 核心设计原则

1. **单 Agent 闭环内核边界**：
   - **内核 = `thunder-agent-loop`**：负责单个 AgentLoop 的自主执行（推理、流式事件、工具调度、上下文裁剪）。不感知任何多 Agent 拓扑、外部调度器或持久化会话库。
   - 宿主（root / daemon / tui）只通过 `start` / `join` / `cancel` 驱动内核，绝不接管内核内部的 turn。
2. **纯粹的传输抽象与 Pi-Bridge**：
   - `thunder-agent-loop` 内置的 `LLMClientTrait` 纯契约抽象，不包含任何网络或特定供应商协议库。
   - 生产环境中统一通过 `thunder-pi-bridge` 将调用代理给 `@earendil-works/pi-ai`，零维护支持多供应商方言与思考模式。
3. **并发写安全保护**：
   - 无论宿主并发多任务还是单 Agent 内多工具并发，写文件操作统一通过 `FILE_MUTATION_LOCKS` 按物理路径互斥排队，杜绝竞态破坏。
4. **确定性轻量宿主**：
   - `thunder-agent-root` 默认加载通用基线插件（会话与技能），并通过关键字意图动态触发高级插件，杜绝昂贵的二次 LLM 分类时延。

---

## 4. 运行与测试

### 环境依赖
- **Rust**: 1.80+
- **Node.js**: 20+

### 一键脚本

```bash
./run.sh          # 启动 thunder-tui 终端应用
./daemon.sh       # 启动 thunder-agent-daemon 守护进程
./test.sh         # 依次测试全工作区 10 个 Crate
```

### 单包精准测试与构建

```bash
# 测试指定包
cargo test -p thunder-agent-loop
cargo test -p thunder-conversation
cargo test -p thunder-agent-daemon

# 全局 Workspace 编译与测试
cargo check --workspace
cargo test --workspace
cargo build --workspace --release
```

---

## 5. 开发规范与代码提交

- **单一仓库原则**：所有开发任务统一在工作区根目录提交，杜绝在各子目录建立独立的 `.git`。
- **提交规范**：采用 Conventional Commits 规范，统一使用英文动词（如 `feat(root): ...`, `refactor(loop): ...`）。
- **CI 门禁**：提交前确保运行 `./test.sh` 全量通过。
