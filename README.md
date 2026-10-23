# ⚡ Thunder Agent

Thunder 是一个采用**纯粹微内核架构（Microkernel Architecture）**的高性能 AI 编码 Agent 框架。

内核仅负责驱动推理与调度循环（ReAct Agent Loop），对具体的操作系统业务工具（文件读写、Bash 执行等）零硬编码。环境交互能力通过双协议工具栈外挂注入，配合前置网关确保大模型服务端 100% KV-Cache 命中率。

---

## 🚀 核心架构亮点

* **纯粹微内核 (`thunder-core`)**：内核只依赖抽象的 `ToolProvider` 与可组合的 `ToolMiddleware` 管线，不包含任何业务 I/O 与硬编码命令。
* **双协议工具通信栈**：
  * **常驻服务**：标准 MCP 协议扩展 `_meta` 上下文信封（`session_id`, `workspace_cwd`, `timeout_ms`），支持跨工作区多会话无缝复用；
  * **本地脚本 (TSP v1)**：零执行开销扫描 `.thunder/tools/` 下单文件脚本前 40 行的 `# @thunder` YAML 元数据，通过独立进程组沙箱并发调用。
* **KV-Cache 护盾与双层暴露网关**：
  * 顶层 `tools` 参数在会话周期内保持静态（仅暴露 7 个核心工具 + `thunder__discover_tools` + `thunder__call_tool` 元工具）；
  * 动态脚本热插拔仅在 Turn 边界以 `<system_notification>` 注入，杜绝修改 API 请求体击穿前缀缓存。
* **执行沙箱与防死锁暴流保护 (`thunder-executor`)**：
  * 使用安全进程组（`process_group(0)`）实现超时级联 `SIGKILL` 强杀与子进程回收，杜绝僵尸进程；
  * 并发双管道消费彻底消除子进程写超 64KB 管道缓冲区导致的死锁，溢出内容无损落盘并保留**头尾各 40%** UTF-8 安全切片。
* **Pi-Style Checkpoint 压缩与缓存保温**：
  * 零分配 CJK 感知 Token 估算器；
  * 阶跃式结构化 Checkpoint 压缩，保留 `Assistant(tool_calls) <-> Tool(result)` 配对不变量；
  * 跨厂商上下文超限报错自愈；
  * 基于精算模型的后台 Prompt Cache 保温器。
* **解耦的能力扩展包**：
  * `thunder-plan`：提供计划模式治理（`write_plan`、`exit_plan_mode`、只读命令词法分析与中间件）；
  * `thunder-ask`：提供人机交互问答工具（`core__ask_user_question` 与抽象 Handler）；
  * `thunder-daemon`：无缝对接 `agent-resume-panel` 桌面端的 NDJSON Stdio 守护进程；
  * `thunder-tui`：基于 Ratatui + Crossterm 的流式交互终端界面。

---

## 📦 工作区 Crate 拓扑

| Crate | 目录 | 职责说明 |
|---|---|---|
| [`thunder-protocol`](crates/thunder-protocol/README.md) | `crates/thunder-protocol` | 统一 Tool Schema、Wire 消息、_meta 上下文信封、流契约与事件总线 |
| [`thunder-executor`](crates/thunder-executor/README.md) | `crates/thunder-executor` | 独立进程组沙箱、并发管道排空、64KB 截断落盘、Scratchpad、MCP 连接池 |
| [`thunder-registry`](crates/thunder-registry/README.md) | `crates/thunder-registry` | Frontmatter 静态解析、ArcSwap 无锁双缓冲快照、200ms 热加载 |
| [`thunder-providers`](crates/thunder-providers/README.md) | `crates/thunder-providers` | RpiAiClient 多协议流式驱动、models.json / auth.json 本地目录解析 |
| [`thunder-std`](crates/thunder-std/README.md) | `crates/thunder-std` | 核心标准工具包 MCP 进程（7 个工具，含内存级 ripgrep 与同路径写锁） |
| [`thunder-core`](crates/thunder-core/README.md) | `crates/thunder-core` | 微内核 ReAct 循环、KV-Cache 网关、Checkpoint 压缩、会话存储 |
| [`thunder-plan`](crates/thunder-plan/README.md) | `crates/thunder-plan` | 计划模式扩展包（write_plan、exit_plan_mode、只读 Shell 证明器） |
| [`thunder-ask`](crates/thunder-ask/README.md) | `crates/thunder-ask` | 人机交互问答扩展包（ask_user_question 与抽象 Handler） |
| [`thunder-daemon`](crates/thunder-daemon/README.md) | `crates/thunder-daemon` | Desktop Sidecar 守护进程（NDJSON 协议 v3，对接 Electron 桌面端） |
| [`thunder-tui`](crates/thunder-tui/README.md) | `crates/thunder-tui` | 交互式终端 UI（流式时间线、运行中 Steer/Pause、Slash 命令） |
| [`thunder-cli`](crates/thunder-cli/README.md) | `crates/thunder-cli` | `thunderctl` 运行时管理（dev / prod / bundled 三态、status 诊断、reload 热重启） |

---

## 🛠 快速上手

### 1. 配置模型凭证

Thunder 优先读取 `~/.thunder/models.json` 与 `~/.thunder/auth.json`。也可以在当前项目的 `.thunder/config.toml` 中配置：

```toml
[agent]
model = "gpt-4o"
api_base = "https://api.openai.com/v1"
max_turns = 16
max_context_tokens = 128000
# 运行预算：0 表示不限。wall_clock_secs 是唯一能停住"确实在进展"的长任务的闸门。
wall_clock_secs = 1800
max_total_tokens = 2000000
max_tool_calls_per_turn = 16
warn_at = 0.75
# 停滞检测阈值（连续多少轮没有新的状态变更 / 新的工具结果才算停滞）
stall_mutation_free_turns = 8
stall_no_novel_turns = 6
# 自愈：停滞时允许重置上下文的次数；整轮重启默认关闭
max_context_resets = 2
max_restarts = 0

[sandbox]
timeout_ms = 30000
max_output_bytes = 65536
# `.thunder/tools/` 的轮询间隔（ms）。0 用内置默认；改大省 IO，改小更快热重载。
debounce_ms = 500
```

### 2. 交互式终端 TUI
```bash
# 启动 TUI 界面（自动挂载核心工具与本地脚本）
./run.sh

# 或使用 cargo 运行
cargo run -p thunder-tui --bin thunder-tui
```

**TUI 快捷键**：
* `Enter`：发送提示词 / 在任务运行中实时插话（Steer）
* `Shift + Tab`：切换 `yolo`（执行模式）与 `plan`（规划模式）
* `Ctrl + P`：在工具执行边界暂停 / 恢复任务
* `Ctrl + C` / `Esc`：取消当前正在运行的任务
* 斜杠命令：`/model`（切换模型）、`/tools`（查看工具快照）、`/mode`、`/new`、`/sessions`、`/compact`、`/clear`、`/quit`

### 3. 启动桌面端 Sidecar 守护进程

```bash
# 启动 NDJSON Stdio 守护进程供桌面客户端连接
./daemon.sh

# 发送测试 Ping
echo '{"method":"ping","id":"1"}' | ./daemon.sh
```

### 4. 命令行直调 (CLI)

```bash
# 查看当前 Catalog 快照中的可用工具
cargo run -p thunder-core -- --list-tools

# 查看本地配置的可用模型列表
cargo run -p thunder-core -- --list-models

# 执行单次任务
cargo run -p thunder-core -- "重构当前模块的单元测试"
```

### 5. 运行时管理（`thunderctl`）

`thunderctl` 管理「跑哪一份二进制」：仓库构建、安装版、还是 Agent Resume 自带版。

```bash
# 一次性安装：构建 4 个二进制并固化到 ~/.cargo/bin
cargo install --path crates/thunder-cli
thunderctl install

# 日常开发：构建 + 把 Desktop 指向本仓库 target/tui-dev
thunderctl dev

# 改完代码让常驻 daemon 原地换心（pid 不变，不重启 App）
thunderctl reload

# 诊断：解析链、运行中进程、三方版本对比、数据量
thunderctl status
```

完整流程（含三态切换、profile 取舍、数据隔离、故障排查）见
[`docs/development.md`](docs/development.md)。

---

## 🧪 质量门禁与测试

```bash
# 与 CI 完全一致的本地门禁（fmt → clippy → test）
bash scripts/ci.sh

# 或分项执行
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

---

## 📖 架构文档

* 完整架构技术规范书请参阅：[`docs/architecture.md`](docs/architecture.md)
* 开发模式、三态切换与热重启指南请参阅：[`docs/development.md`](docs/development.md)
