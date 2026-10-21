# Thunder Agent 架构设计与技术规范书

## 1. 架构愿景与设计哲学

Thunder 采用**微内核架构（Microkernel Architecture）**。内核仅负责驱动推理与调度循环（ReAct Agent Loop），不硬编码任何具体业务工具（如文件读写、Bash 执行等）。环境交互能力均通过外挂协议注入。

```text
+---------------------------------------------------------------------------------------+
|                                     thunder-core                                      |
|                                                                                       |
|  +------------------------+  +-------------------------+  +------------------------+  |
|  | Context & Checkpoint   |  |   ReAct Agent Loop      |  | Composable Middleware  |  |
|  | Compaction + Warmer    |  | (LoopGuard/Steer/Pause) |  | Pipeline (Pluggable)   |  |
|  +------------------------+  +-------------------------+  +------------------------+  |
|                 │                         │                           │               |
|                 ▼                         ▼                           ▼               |
|  +---------------------------------------------------------------------------------+  |
|  |                        KV-Cache Shield & Gateway Router                         |  |
|  |  Static Top-Level: [core__*, thunder__discover_tools, thunder__call_tool]       |  |
|  |  Dynamic Disclosure: Turn-boundary <system_notification>[Registry Update]       |  |
|  +---------------------------------------------------------------------------------+  |
+-----------------│─────────────────────────────────────│───────────────────────────────+
                  │                                     │
                  ▼ (LLMClientTrait)                    ▼ (Lock-Free Snapshot Load)
    +---------------------------+         +-----------------------------------------+
    |     thunder-providers     |         |            thunder-registry             |
    |  - RpiAiClient (HTTP SSE) |         |  - ArcSwap<CatalogSnapshot> (BTreeMap)  |
    |  - ~/.thunder/models.json |         |  - Shadowing: Script > MCP > Core       |
    |  - ~/.thunder/auth.json   |         |  - notify-debouncer-mini (200ms)        |
    |  - OpenAI / Anthropic     |         |  - Top-40-line @thunder YAML parser     |
    +---------------------------+         +-----------------------------------------+
                                                        │
                                                        ▼ (ToolProvider Trait)
                                          +-----------------------------------------+
                                          |            thunder-executor             |
                                          |  - Process Group Sandbox (setpgid/kill) |
                                          |  - Concurrent Pipe Drain & Flood Guard  |
                                          |  - ScratchpadManager (.thunder/scratch) |
                                          |  - StdioMcpClient Pool + _meta Envelope |
                                          +-----------------------------------------+
                                             │                 │                 │
                                  Stdio JSON-RPC    Stdio JSON-RPC     Stdin JSON / Env
                                  (_meta.cwd)       (Schema Path Fix)  (One-Shot Spawn)
                                             │                 │                 │
                                             ▼                 ▼                 ▼
                                    +---------------+  +---------------+  +---------------+
                                    |  thunder-std  |  | External MCP  |  | Local Scripts |
                                    | (7 Core Tools)|  | (Git/DB/etc.) |  |(.thunder/tools|
                                    +---------------+  +---------------+  +---------------+
```

### 核心设计原则

* **Kernel Purity（内核纯粹性）：** `thunder-core` 仅依赖基础网络库与 LLM 驱动，不包含任何业务 I/O。标准工具集以独立二进制 `thunder-std` 作为标准 MCP 外部进程挂载。
* **Zero-Restart Hot-Reload（零重启热插拔）：** 支持常驻守护进程与本地脚本的运行时动态发现与更新，无需重启 CLI / Daemon 会话。
* **KV-Cache Preservation（缓存友好）：** 动态加载工具不频繁变更 LLM 顶层 `tools` 参数，避免模型前缀 KV Cache 频繁失效。
* **Fail-Safe Isolation（故障隔离）：** 单个工具崩溃、管道破裂或超时不扩散至 Agent 状态机，具备超时杀灭进程组、并发管道排空防死锁与错误归一化机制。

---

## 2. 系统分层与模块拓扑

### 2.1 Workspace 拓扑结构

```text
thunder/
├── Cargo.toml                    # Workspace 根配置 (resolver = "2")
├── rust-toolchain.toml           # 固定 Rust 1.98.0
├── daemon.sh                     # Desktop Sidecar 启动脚本
├── run.sh                        # TUI 启动脚本
├── .thunder/
│   ├── config.toml               # 运行时配置
│   └── tools/                    # TSP v1 本地单文件脚本目录
└── crates/
    ├── thunder-protocol/         # 纯契约层：Schema、Wire消息、RPC信封、流接口、事件总线
    ├── thunder-executor/         # 执行隔离层：进程组沙箱、防死锁截断、Scratchpad、MCP连接池
    ├── thunder-registry/         # 动态注册表：Frontmatter解析、ArcSwap双缓冲、200ms热加载
    ├── thunder-providers/        # 多厂商模型层：RpiAiClient、models.json/auth.json、探测与缓存定价
    ├── thunder-std/              # 核心标准库：独立 MCP Server 二进制 (7个工具)
    ├── thunder-core/             # 微内核调度层：ReAct循环、KV-Cache网关、Checkpoint压缩、会话存储
    ├── thunder-plan/             # 计划模式扩展包：只读Shell证明器、write_plan、exit_plan_mode
    ├── thunder-ask/              # 人机问答扩展包：ask_user_question 工具、抽象问答 Handler
    ├── thunder-daemon/           # Desktop Sidecar：NDJSON Stdio 协议 (v3)、会话与事件桥接
    └── thunder-tui/              # 交互式终端 UI：Ratatui + Crossterm、流式卡片、Steer/Pause
```

### 2.2 模块职责与依赖约束

依赖关系严格遵循单向无环原则：

```text
thunder-protocol (0 内部依赖)
  ▲
  ├── thunder-executor   (依赖: protocol)
  ├── thunder-registry   (依赖: protocol, executor)
  ├── thunder-providers  (依赖: protocol)
  ├── thunder-std        (依赖: protocol, executor)
  ├── thunder-core       (依赖: protocol, executor, registry, providers)
  ├── thunder-plan       (依赖: protocol, core)
  ├── thunder-ask        (依赖: protocol, core)
  ├── thunder-daemon     (依赖: protocol, executor, registry, providers, core, plan, ask)
  └── thunder-tui        (依赖: protocol, executor, registry, providers, core, plan, ask)
```

---

## 3. 双协议工具通信栈（Wire Protocols）

### 3.1 常驻服务：MCP + Context Envelope (`_meta`)

适用于 `thunder-std` 以及社区生态提供的所有标准 MCP Server。

```json
{
  "jsonrpc": "2.0",
  "id": "req-98210",
  "method": "tools/call",
  "params": {
    "name": "read_file",
    "arguments": {
      "path": "src/lib.rs"
    },
    "_meta": {
      "session_id": "sess-6f3b",
      "workspace_cwd": "/workspace/project",
      "timeout_ms": 30000
    }
  }
}
```

* **`thunder-std` 处理规则：** 优先采用 `_meta.workspace_cwd` 作为相对路径的基础路径，并以 `_meta.timeout_ms` 实施调用超时熔断。
* **第三方 MCP 处理规则：** `thunder-executor` 根据工具的 JSON Schema，对显式声明为文件路径的字段（`path`, `file_path`, `cwd` 等）自动转换为基于 `workspace_cwd` 的绝对路径后再发出调用。

### 3.2 瞬态服务：Thunder Script Protocol (TSP v1)

适用于放入 `.thunder/tools/` 的单文件自动化脚本（Shell, Python, Node.js 等）。

#### 静态元数据发现（Frontmatter Specification）

无需执行脚本，仅扫描文件头部前 40 行文本中的声明块：

```bash
#!/usr/bin/env bash
# @thunder
# name: git_squash_feature
# description: 自动压缩当前分支的所有特性提交并推送到远端
# parameters:
#   type: object
#   properties:
#     base_branch:
#       type: string
#       description: 目标基准分支，例如 main
#   required: [base_branch]
# @end

set -euo pipefail
# 脚本业务逻辑...
```

* **输入分发：** 参数通过 `stdin` 发送完整 JSON 并立即关闭写端（EOF）；注入环境变量 `THUNDER_CWD`、`THUNDER_SESSION_ID`；工作目录绑定到 `workspace_cwd`。
* **输出消费：** `ExitCode == 0` 取 `stdout`；`ExitCode != 0` 捕获 `stderr`，自动剥离 ANSI 终端颜色代码，封装为规范化的 `ToolExecutionError::NonZeroExit`。

---

## 4. 动态发现与 KV-Cache 护盾

### 4.1 双缓冲并发模型与命名空间覆盖

* 运行时使用 `Arc<ArcSwap<CatalogSnapshot>>` 维护当前可用工具清单。
* 扫描线程通过 `notify-debouncer-mini` 捕获 `.thunder/tools/` 文件增删改，防抖时间设置为 **200ms**。
* 命名空间与内部 URI：
  * 核心工具：`core://read_file`（暴露名 `core__read_file`）
  * 外部 MCP：`mcp://git/commit`（暴露名 `mcp__git__commit`）
  * 本地脚本：`script://git_squash_feature`（暴露名 `script__git_squash_feature`）
* **覆盖机制（Shadowing）：**
  $$\text{Local Script (script://)} > \text{External MCP (mcp://)} > \text{Core (thunder-std)}$$

### 4.2 提示词缓存保护机制（KV-Cache Shield）

在 API 请求体中频繁增删工具会导致大模型服务端前缀缓存彻底失效。Thunder 采用**双层暴露体系**：

```text
LLM API 顶层 tools 声明 (固定结构，保证 100% KV-Cache 命中)
├── core__read_file
├── core__write_file
├── core__edit_block
├── core__bash
├── core__grep
├── core__find
├── core__ls
├── thunder__discover_tools (元工具: 搜索/拉取动态工具完整 Schema)
└── thunder__call_tool      (元工具: 统一网关路由分发)
```

1. **顶层注册：** 仅将高频核心工具及 2 个元工具在会话初始注册至顶层 `tools` 参数中，会话周期内保持字节级静态不变。
2. **渐进式披露：** 本地脚本或第三方 MCP 工具不直接灌入顶层 `tools`。当文件变更或新 MCP 挂载时，在历史消息末尾插入轻量级的系统状态通知：
```xml
<system_notification>
[Registry Update] 工具已挂载: `script__git_squash_feature` (自动压缩分支提交)。
可直接通过 `thunder__call_tool(name="script__git_squash_feature", arguments={...})` 调用。
</system_notification>
```

---

## 5. 执行沙箱与安全隔离机制

### 5.1 进程组沙箱与超时强杀

* 使用 `cmd.process_group(0)` 为工具及其衍生子进程创建独立进程组（Process Group）。
* 超时或取消时，向 `-pid` 进程组发送 `SIGKILL` 级联杀死所有子孙进程，并调用 `child.wait().await` 回收描述符，彻底杜绝僵尸进程。

### 5.2 防死锁并发流排空与头尾 40% 截断落盘

* 传统的顺序读取在子进程输出超 64KB OS 管道缓冲区时会发生 `write(2)` 永久阻塞死锁。
* `thunder-executor` 采用双独立异步任务并发排空 `stdout` 与 `stderr` 直至 EOF。
* 超过 64KB 时：
  * 保留 **头 40% + 尾 40%**（UTF-8 字符边界安全对齐，确保末尾的错误堆栈不丢失）；
  * 完整输出无损落盘至 `/tmp/thunder_output_*.log` 或 `~/.thunder/scratchpad/`。

### 5.3 故障归一化（Error Normalization）

统一将底层异构执行结果映射为标准结构体：

```rust
pub enum ToolExecutionResult {
    Success { output: String },
    ToolError { code: i32, stderr: String, is_retryable: bool },
    SystemFault { reason: String },
}
```

---

## 6. 上下文管理、Checkpoint 压缩与缓存保温

1. **零分配 CJK 感知 Token 估算：** 针对中文字符（`~1.5 tokens/字`）、ASCII（`~3.7 chars/token`）和多模态图片进行校准估算。
2. **Pi-style 结构化 Checkpoint 压缩：**
   * 阶跃式触发：未达 `max_context_tokens - reserve_tokens` 时历史消息一字不改；
   * 安全切分边界：自动跳过 `Tool` 消息，绝不拆散 `Assistant(tool_calls) <-> Tool(result)` 配对；
   * 结构化摘要：提取 `<read-files>` 与 `<modified-files>`，替换为 `context_checkpoint` 消息，并支持 `emergency_trim` 兜底。
3. **Prompt Cache 保温器 (`cache_warmer`)：**
   * 基于模型价格精算期望节省：$\text{expected} = P(\text{continue}) \times \text{miss\_cost} - \text{warm\_cost}$；
   * 当期望收益 $\ge \$0.05$ 时，在 90% TTL 处自动发送 `max_tokens = 1` 请求为缓存续命。

---

## 7. 宿主与扩展能力包

1. **`thunder-std`**：提供 7 个标准核心工具，内置内存级 ripgrep、glob 查找、大文件行级流式分页（>10MB 防 OOM）与同路径互斥锁 `file_lock_for`。
2. **`thunder-plan`**：独立 In-Process 扩展包，提供静态只读 Shell 命令分析、`write_plan`、`exit_plan_mode` 与 `PlanModeMiddleware`（只读沙箱拦截）。
3. **`thunder-ask`**：独立 In-Process 扩展包，提供 `ask_user_question` 交互问答工具与 `UserQuestionHandler` 抽象。
4. **`thunder-daemon`**：基于 Stdio 的 NDJSON 协议（v3）Sidecar 守护进程，无缝对接 `agent-resume-panel` Desktop。
5. **`thunder-tui`**：基于 Ratatui + Crossterm 的终端交互 UI，提供流式卡片时间线、Slash 命令与运行期实时插话（Steer）。
