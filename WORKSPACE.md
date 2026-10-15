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
- 各子 Crate 目录**不再保留**独立的 `LICENSE`、`run.sh` 或 `.gitignore` 副本：这些属于工作区根级资产，通过 `[workspace.package]` 继承与根级忽略规则统一治理

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
├── WORKSPACE.md                  # 中文工作区规范（本文件）
├── WORKSPACE_en.md               # 英文工作区规范
├── LICENSE
├── Cargo.toml                    # 统一工作区配置（10 个成员包）
├── Cargo.lock                    # 统一共享锁文件
├── rust-toolchain.toml           # 固定构建链（1.98.0，与 CI 同步 pin）
├── run.sh                        # 启动 TUI 终端
├── daemon.sh                     # 启动 STDIO Sidecar 守护进程
├── test.sh                       # 全工作区测试套件脚本
├── .github/workflows/            # CI（fmt / clippy / check-version / test）与 release
├── .thunder/                     # Thunder 自身配置（用户级 ~/.thunder/ 与项目级同构）
├── .arp/                         # 归 agent-resume-panel（面板配置 + 跨会话共享产物），Thunder 不读
├── docs/                         # architecture.md、permission-structure.md、migration-0.4.md
├── examples/electron/            # Electron 侧车集成示例
├── scripts/check-version.py      # 版本一致性门禁（CI / release / test.sh 三处调用）
├── thunder-agent-loop/           # Agent 内核：单 Agent 闭环执行引擎 + 工具执行入口 + trace
├── thunder-agent-pack-code/      # 编码能力包：6 个内建工具 + 路径作用域判定
├── thunder-agent-runtime/        # 无头运行时：以 JSONL 协议驱动内核
├── thunder-agent-providers/      # LLM Catalog、配置解析与进程内流式传输
├── thunder-agent-skills/         # Skill 解析、注册与全局缓存
├── thunder-agent-mcp/            # MCP 客户端与服务端生态及动态工具/插件桥接
├── thunder-agent-root/           # 微内核 host，按任务动态挂载插件
├── thunder-agent-daemon/         # STDIO Sidecar 守护进程（供 Electron / 外部前端集成）
└── thunder-agent-core/           # 核心业务组件集合
    ├── conversation/             # 会话与 Turn 存储（thunder-conversation）
    └── tui/                      # 终端界面（thunder-tui）
```

---

## 3. 依赖关系与架构分层

全工作区所有包之间均使用 **Path 依赖**，严禁使用 git URL 交叉引用。

各 crate 直接依赖的**内部** crate（逐条抄自各自的 `Cargo.toml`；`loop` 一栏为空是硬约束，不是遗漏）：

| Crate | 依赖的内部 crate |
|---|---|
| `thunder-agent-loop` | —（内核不依赖任何宿主） |
| `thunder-agent-pack-code` | `loop` |
| `thunder-agent-providers` | `loop` |
| `thunder-agent-skills` | `loop` |
| `thunder-agent-mcp` | `loop` |
| `thunder-conversation` | `loop` |
| `thunder-agent-runtime` | `loop`、`providers` |
| `thunder-agent-root` | `loop`、`pack-code`、`providers`、`skills`、`mcp`、`conversation` |
| `thunder-agent-daemon` | `loop`、`providers`、`skills`、`mcp`、`conversation`、`root` |
| `thunder-tui` | `loop`、`pack-code`、`providers`、`skills`、`mcp`、`conversation`、`root` |

> 这张表取代了早先的手绘箭头图：那张图把 `root` / `daemon` / `tui` 画成只依赖 `loop`（和 `root`），漏掉了 `pack-code`、`providers`、`skills`、`mcp`、`conversation` 这几条真实存在的边。分层结论不变——`loop` 是叶子，`root` 是微内核宿主，`tui`/`daemon` 是并列的两个最上层二进制。

### 核心设计原则

1. **单 Agent 闭环内核边界**：
   - **内核 = `thunder-agent-loop`**：负责单个 AgentLoop 的自主执行（推理、流式事件、工具调度、上下文裁剪）。不感知任何多 Agent 拓扑、外部调度器或持久化会话库。
   - 宿主（root / daemon / tui）只通过 `start` / `join` / `cancel` 驱动内核，绝不接管内核内部的 turn。
2. **纯粹的传输抽象与进程内直连**：
   - `thunder-agent-loop` 内置的 `LLMClientTrait` 纯契约抽象，不包含任何网络或特定供应商协议库。
   - 生产环境中由 `thunder-agent-providers` 的 `RpiAiClient` 直接发 HTTP 流式请求（`rpi-ai`），不起子进程、不依赖宿主机装 Node；不支持的方言明确报错。
3. **并发写安全保护**：
   - 无论宿主并发多任务还是单 Agent 内多工具并发，写文件操作统一通过 `FILE_MUTATION_LOCKS`（`thunder-agent-pack-code/src/builtin/fs.rs`）按物理路径互斥排队，杜绝竞态破坏；内核本身不持有这把锁。
4. **确定性轻量宿主**：
   - `thunder-agent-root` 默认加载通用基线插件（会话、技能与长期记忆），并通过关键字意图动态触发高级插件，杜绝昂贵的二次 LLM 分类时延。
5. **策略在宿主，不在内核**：
   - 内核（`thunder-agent-loop`）不带任何审批意见：它只提供**一个**统一步骤的工具执行漏斗（`ToolExecutor`）、洋葱拦截器流水线、`ToolGate` 钩子、`ToolScope` 钩子与不可绕过的 guard。未安装 `ToolGate` 即"除 guard 外全放行"，无头宿主因此不需要审批策略也能跑。
   - 审批模式（`plan` / `ask` / `acceptEdits` / `yolo`）住在 `thunder-agent-root/src/plugins/mode.rs` 的 `ModePlugin` 里，可被宿主卸载。
6. **配置归 .thunder/，不碰 .arp**：
   - `.arp/` 属于 `agent-resume-panel`（面板配置 + 共享产物），Thunder 不读取其任何内容。
   - Thunder 自身的项目状态统一在 `.thunder/`：`config.json`（agent 配置）、`mcp.json`（MCP servers）、`THUNDER.md` + `memory/*.md`（长期记忆）、`plans/`（plan 模式产出的计划文件）。用户级 `~/.thunder/` 提供同名默认值，按 用户级 → 项目级 → `*.local.*` 覆盖。
   - 用户级数据根可用 `THUNDER_CONFIG_DIR`（或 `--config-dir <path>`）整体重定向，默认 `~/.thunder`。它统管 thunder **全部**自有状态 —— 上面这些之外还有 `conversations/`、`scratchpad/`、`logs/`、`models.json`/`auth.json`/`models_metadata.json`、`permissions.json`（同意过的 always allow 规则） —— 所以宿主能把一个实例完全收进自己的目录。解析规则集中在 `thunder-agent-loop/src/core/paths.rs`，跨工具路径（`~/.agents`、`~/.pi`、shell rc 取 key）不受影响，仍取真实 HOME。

---

## 4. 运行与测试

### 环境依赖
- **Rust**: 最低 1.80（`Cargo.toml` 的 `rust-version`）；仓库用 `rust-toolchain.toml` 固定 **1.98.0**，与 CI 及 lint 门禁同步
- 不需要 Node.js：模型调用在进程内完成

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
- **CI 门禁**：提交前确保本地跑过 `./test.sh`（全量测试）；CI 另有 fmt → clippy → `scripts/check-version.py` 三道 lint 门禁。
