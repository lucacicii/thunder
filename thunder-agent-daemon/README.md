# Thunder Daemon (Sidecar for Electron & Desktop UI)

> **面向 Electron、桌面应用及外部宿主的轻量级 STDIO Sidecar 守护进程**

[English](README_en.md) | [简体中文](README.md)

`thunder-daemon` 是专为桌面客户端设计的常驻子进程服务。通过标准输入输出（STDIO）进行高性能 NDJSON 消息交互，将完整的 Agent 执行、工具调用、插件装配与多轮会话能力无缝嵌入外部宿主。

---

## 🚀 核心架构与特性

- **零端口冲突与天然绑定**：纯 STDIO 管道通信，不监听任何 TCP/HTTP 端口，无端口占用或防火墙拦截风险；当 Electron/父进程退出或崩溃时，STDIN 管道自动触发 EOF，`thunder-daemon` 毫秒级优雅退出，杜绝后台僵尸进程。
- **并发控制信号量与背压调度**：内置异步任务信号量（`THUNDER_MAX_CONCURRENT_TASKS` 环境变量可调，默认 `8`），限制并发重度推理任务数量，防止本地资源与模型并发配额耗尽。
- **异步 STDOUT Actor（消息零交错）**：所有事件流推送统一通过专用 STDOUT Actor 与带缓冲的 MPSC Channel 排队发送。在高并发多任务流式输出时，**确保每一行 NDJSON 绝对完整且不出现字符交错**。
- **权威历史重载与分层轨迹保留**：任务启动前自动从磁盘存储重载最新权威会话历史，保证多端或重连时消息不错乱；内存轨迹采用分层保留策略——高频微增量（`TokenDelta` / `ReasoningDelta` / `ToolCallChunk`）实时推送至 STDOUT 但**不落入**内存轨迹，而是**计入** `dropped_deltas`（丢弃是计数的，不是静默的），杜绝常驻守护进程内存无限增长。trace 文件另含 `tool_traces`（上限 2000）与 `tool_decisions`（被挤出的记录保留判定本身）。
- **协作式暂停（Pause）与反问气泡（Ask User）**：
  - `pause_task` 在下一个工具派发边界安全挂起，不中断进行中的写入操作；`resume_task` 恢复运行。
  - `ask_user_question` 允许 Agent 向用户发起阻塞式提问，宿主以气泡卡片渲染并通过 `answer_question` 答复继续任务。
- **日志隔离**：所有系统及调试日志强制输出至 `STDERR`，确保 `STDOUT` 数据流百分之百为纯粹合法的 NDJSON 协议帧。

---

## 🛠️ 启动与调试

```bash
# 1. 启动 daemon
./daemon.sh

# 2. 发送测试 Ping
echo '{"method":"ping","id":"1"}' | ./daemon.sh

# 3. 查询可用模型规格
echo '{"method":"list_models","id":"2"}' | ./daemon.sh
```

---

## 📡 协议说明 (NDJSON over STDIN/STDOUT)

### 1. 宿主请求指令（STDIN）

每行一条合法 JSON：

```json
{"method":"ping","id":"req-1"}
{"method":"list_models","id":"req-2"}
{"method":"run_task","id":"req-4","task_id":"task-1","prompt":"帮我检查当前目录的 git 状态","session_id":"sess-001"}
{"method":"cancel_task","id":"req-5","task_id":"task-1"}
{"method":"pause_task","id":"req-6","task_id":"task-1"}
{"method":"resume_task","id":"req-7","task_id":"task-1"}
{"method":"answer_question","id":"req-8","question_id":"task-1:q1","answers":{"要重构哪一层？":"渲染层"}}
{"method":"answer_ui","id":"req-9","request_id":"ui_1234_1_99","value":"Allow once"}
{"method":"answer_ui","id":"req-10","request_id":"ui_1234_2_100","confirmed":false}
{"method":"answer_ui","id":"req-11","request_id":"ui_1234_3_101","cancelled":true}
{"method":"get_permission_state","id":"req-13","session_id":"sess-001"}
{"method":"set_mode","id":"req-14","session_id":"sess-001","mode":"plan"}
{"method":"reload_integrations","id":"req-15"}
```

### 2. 守护进程输出（STDOUT）

- **请求响应**：`{"type":"response","id":"req-1","success":true,"data":{...}}`
- **流式增量事件**：`{"type":"observed_event","task_id":"task-1","event":{...}}`
- **反问挂起（需用户回答）**：`{"type":"user_question","task_id":"task-1","question_id":"task-1:q1","questions":[{"question":"...","options":[{"label":"..."}]}]}`
- **审批档位变了（推送）**：`{"type":"mode_changed","session_id":"sess-001","mode":"plan","previous":"yolo","source":"user"}`——`source` 为 `user` / `plan_approved` / `run_option`；变化发生在某次 run 内时多带 `task_id`。**同值不播报**，所以收到它就一定是真的变了。
- **任务已暂停**：`{"type":"task_paused","task_id":"task-1","reason":"..."}`
- **任务成功完成**：`{"type":"task_completed","task_id":"task-1","session_id":"...","final_content":"...","finish_reason":"Done"}`
- **任务失败**：`{"type":"task_failed","task_id":"task-1","error":"..."}`

### 3. 通用弹窗子协议（ui_request / answer_ui）

与 `ask_user_question` 并列的第二类人机交互，语义对齐 pi 的 `extension_ui_request`。
**只传数据，不传组件** —— 同一套协议可被 TUI、Electron 面板或无头宿主复用。

守护进程 → 宿主（阻塞，直到 `answer_ui` 抵达或 `timeout_ms` 超时）：

```json
{"type":"ui_request","request_id":"ui_1234_1_99","task_id":"task-1","session_id":"sess-001",
 "source":"host","ui":"select","title":"Run bash","options":["Allow once","Always allow","Deny","Deny with reason"],"timeout_ms":600000}
{"type":"ui_request","request_id":"ui_1234_2_100","source":"host","ui":"confirm","title":"Proceed?","message":"将写入 3 个文件"}
{"type":"ui_request","request_id":"...","source":"host","ui":"input","title":"输入值","placeholder":"..."}
{"type":"ui_request","request_id":"...","source":"host","ui":"editor","title":"编辑","prefill":"..."}
{"type":"ui_request","request_id":"...","source":"host","ui":"review","title":"Approve this plan?","path":".thunder/plans/sess-001.md","content":"# Plan\n…","options":["Yes, and auto-accept edits","Yes, but ask before each edit","Yes, fully autonomous","No, keep planning"]}
```

宿主 → 守护进程（`request_id` 必须原样回传）：

```json
{"method":"answer_ui","id":"req-9","request_id":"ui_1234_1_99","value":"Allow once"}
{"method":"answer_ui","id":"req-10","request_id":"ui_1234_2_100","confirmed":true}
{"method":"answer_ui","id":"req-11","request_id":"ui_...","cancelled":true}
```

响应体固定回 `{"request_id":"...","delivered":true|false}`；`delivered:false` 表示该
`request_id` 未知或已过期（已超时），此时**不应**报错，迟到的答复会被静默丢弃，
以免落到后续的同名弹窗上。

**`source` 字段是安全相关的**：

| `source` | 含义 | 宿主必须做到 |
|----------|------|--------------|
| `host` | 宿主自身发起（如权限审批） | 使用**插件无法伪造的专用样式**渲染；用户的选择才算授权决定 |
| `plugin` | 插件发起 | 明确标注插件名；**不得**将其视为授权依据 |

请求 id 由守护进程签发（进程号 + 计数器 + 纳秒时间戳），插件无法自选，因此无法
伪造或重放一次审批。`request_id` 未出现在 `timeout_ms` 字段时表示无自定义超时，
守护进程按 60s 兜底。

**超时一律 fail-closed**：超时、面板断开、无人应答，全部解析为“已取消”，
调用方必须把“已取消”读作“拒绝”。`confirm` 永远不会因为超时而返回 `true`。

其他两类 fire-and-forget 消息（宿主无 UI 时可直接丢弃）：

```json
{"type":"ui_notice","source":"plugin","message":"Command blocked by user","level":"warning"}
{"type":"ui_status","key":"my-ext","text":"Turn 3 running..."}
{"type":"ui_status","key":"my-ext"}
```

---

## 🔒 审批模式（Approval Mode）

0.4 起，daemon **不再有档位（tier）**。工具始终全量注册，能不能跑由**模式**决定；
模式的载体是一个可卸载的宿主插件（`ModePlugin`），内核只提供 `ToolGate` 钩子。

### 四种模式

| `mode` | 读 | 工作区内写 | 工作区外写 | shell | 说明 |
|--------|:---:|:---:|:---:|:---:|------|
| **`plan`** | 允许 | **拒绝** | **拒绝** | **只读命令允许，其余询问** | 只读调研 + 写计划文件；`write_plan` / `exit_plan_mode` 是唯一出口 |
| **`ask`** | 允许 | **询问** | **询问** | **询问** | 标准安全防御：读免问，其余弹窗 |
| **`acceptEdits`** | 允许 | **允许** | **询问** | **询问** | 工作区内编辑免问，越界与命令仍问 |
| **`yolo`** | 允许 | 允许 | 允许 | 允许 | 全自动（默认值，与 0.3 的 `never` 行为一致） |

旧名仍可解析：`plan`；`ask` ← `default`/`manual`/`mutations`/`writes`/`always`/`all`；
`acceptEdits` ← `accept_edits`/`shell_only`/`ask_shell`；
`yolo` ← `never`/`none`/`bypass`/`bypass_permissions`。
**注意 `plan` 不再映射到 `never`**（0.3 的那条映射是错的）。

### 三条硬规则

1. **守卫不可被覆盖。** 危险命令（`mkfs*`、`dd if=`、`rm -rf /`、fork bomb）与
   关键路径（凭据库、`.git`、系统目录）在**任何**模式（含 `yolo`）下都拒绝，
   模式改不了它。
2. **路径判定只用来放宽，不用来收紧。** 作用域（`ToolScope`）只把"问"降级成"允许"，
   **永远不会**把"允许"升级成"拒绝" —— 新增判定最多让人多被问一次。
3. **询问是 fail-closed 的。** 无面板 / 超时 / 用户关闭 / 答复不在选项集内 → 一律按
   **拒绝**处理。拒绝会作为**工具结果**回灌给模型（附带"什么都没发生"的 ground truth），
   而不是抛异常，避免模型以为成功而反复重试。

### 默认值是 `yolo`（即"不询问"）

审批门是 **opt-in** 的。原因很实际：门的语义是 fail-closed，
无面板宿主上每个弹窗都会被判为"取消"＝"拒绝"。若默认 `ask`，
升级后所有无面板用户的**每一次写文件、每一条 shell**都会被静默拒绝。
默认值可被 `.thunder/config.json` 的 `agent.defaultMode` 覆盖。

### 设置模式

```json
{"method":"set_mode","id":"req-14","session_id":"sess-001","mode":"plan"}
```

- 响应回 `{"session_id":"...","mode":"plan"}`；未知模式回 `success:false` +
  `unknown mode '…': expected one of plan | ask | acceptEdits | yolo`，
  **绝不静默回落到默认模式**。
- 模式按 `session_id` 记忆（上限 FIFO 淘汰最老的会话），**下一次调用立刻生效**。
- `run_task` 也接受可选 `mode` 字段；它同样写进该会话的模式（不是只作用于本次
  run），该会话后续的 run 继续沿用。
- **档位真的变了会推一条 `mode_changed`**（§2），顺序是**先 `response` 再推送**——
  一次请求读一行的客户端因此仍然先读到它要的那帧。三种来源（`source`）都走同一个帧：
  客户端调的 `set_mode`（`user`）、`run_task` 钉的档位（`run_option`）、
  `exit_plan_mode` 被批准（`plan_approved`，这条 daemon 只能从 run 的事件流里得知）。
  **同值不播报**：宿主每轮 run 开始都会重设一次档位，若同值也播报，每轮都会冒一帧。
  对一个尚不存在的会话调 `set_mode` 是**以该档位创建**它，因此也没有转变可播报——
  确认来自 `response` 的 `data.mode`。
- `get_permission_state` 可查看当前模式与已记住的「always allow」规则。
  daemon 建的会话把规则**落盘**在 `<数据根>/permissions.json`（`ModePlugin::with_persisted_rules`），
  所以「always allow」跨会话有效；删/改这个文件即可撤销（下次建会话生效）。
  注意它对**从未 run 过**的会话是按 `Mode::default()`（`yolo`）创建插件的，
  而首轮 `run_task` 走的是 `.thunder` 的 `agent.defaultMode`；两者可能不同，
  首轮 run 的 `mode_changed`（`run_option`）会把真实档位推过来。

### 交互弹窗长这样

```json
{"type":"ui_request","request_id":"ui_...","source":"host","ui":"select",
 "title":"Run bash","options":["Allow once","Always allow","Deny","Deny with reason"]}
```

- 弹窗**串行**投递：同一批并行工具调用不会叠出多个框，避免批准错对象。
- `Allow once` 只对**当前这一次调用**生效。
- `Always allow` 只在能推导出**窄规则**时出现（如 `git status`、`src/lib.rs`），
  且规则对 shell 命令做**链接符过滤**：`git status` 永远不会授权
  `git status && rm -rf /`。没有 `path` 参数的非 shell 工具**不提供**该选项。
- `Deny with reason` 会追问一句，原因原文回灌给模型。
- 计划审批走 `ui:"review"`（0.4 新增），携带计划文件路径与正文。

### 在哪里生效

判定只有**一个**入口，而且它在**唯一**的工具执行入口里面
（`ToolExecutor::run_tool`）：

```text
ToolExecutor::run_tool
  ├─ 取消检查
  ├─ 作用域   ToolScope（宿主回答"打到哪儿"）
  ├─ 守卫     危险命令 + 关键路径  ← 在判定之前，且不咨询判定
  ├─ 判定     ToolGate::decide（ModePlugin）
  ├─ 弹窗     内核发起（串行、fail-closed）
  ├─ 执行     ToolRegistry::execute_tool_call
  └─ trace    每次调用恰好一条，落盘前脱敏
```

**没有装 gate 时（除守卫外）全部放行** —— 这正是嵌入型无头宿主依赖的红线。

### 插件也是同一条路径

Rust 插件的 `ctx.tool_slot().invoke(...)` 不再自己实现能力，而是拼一个
`ToolExecutionContext`（带 `caller = 插件 id`）后走**同一个 `run_tool`**。因此：

- 插件的调用受**同一条守卫**与**同一个模式**约束；
- 需要审批时，弹窗标题显示 `Plugin <id> requests: …`，插件无法伪装成助手发起；
- 插件发起的调用**同样留下 trace**。

## 🔄 状态机流转与交叉行为

```text
               ┌──────────┐
               │ Running  │
               └────┬─────┘
          pause_task│   ▲ resume_task
          (工具调度边界)│   │
                    ▼   │
               ┌──────────┐
               │  Paused  │
               └──────────┘
                    ▲
                    │ 答复完成且此前曾收到 pause
                    │
         ┌─────────────────────┐
         │ WaitingUserInput    │ ◄── agent 调用 ask_user_question 工具
         │ (user_question 阻塞) │ ──► answer_question 答复后恢复 Running
         └─────────────────────┘
```

1. **`pause_task` 生效边界**：位于工具调度边界前。若当前已有工具正在写文件，该工具会完整跑完，并在派发下一个工具前进入挂起。
2. **反问期间收到 pause**：工具继续等待人类输入；人类提交答复后，该工具完成返回，任务随即在下一个调度边界冻结在 `Paused`。
3. **取消的绝对优先**：任何状态下收到 `cancel_task`，任务立刻终止并释放所有等待资源。

### 运行状态怎么读

「现在在干什么」不是从上面这些帧拼出来的：`thunder-agent-root` 的 `StatusPlugin` 会随 `observed_event` 流推送 `{"type":"custom","kind":"agent_status","payload":{...}}`，阶段为 `idle | thinking | streaming | tool | awaiting_user | paused | compacting | done | error`，带上当前工具名与轮次。**只在跃迁时推**（按 `seq` 去重），所以消费方收到的每一帧都代表真的变了。

- `paused` 与 `awaiting_user` 事件流里没有（内核那时一条事件都不发），由 daemon 自己报给插件：`pause_task`/`resume_task` 报前者、`AwaitingGuard` 在审批/提问弹窗存续期间报后者——两者同时成立时取最新进入的那个。
- 协议版本不用跟着动：`custom` 对 NDJSON 协议是透明的，不认识这个 kind 的客户端直接忽略即可。

---

## 📄 开源协议

本项目采用 [Apache License 2.0](../LICENSE) 开源许可证。
