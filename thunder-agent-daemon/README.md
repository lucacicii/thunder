# Thunder Daemon (Sidecar for Electron & Desktop UI)

> **面向 Electron、桌面应用及外部宿主的轻量级 STDIO Sidecar 守护进程**

[English](README_en.md) | [简体中文](README.md)

`thunder-daemon` 是专为桌面客户端设计的常驻子进程服务。通过标准输入输出（STDIO）进行高性能 NDJSON 消息交互，将完整的 Agent 执行、工具调用、插件装配与多轮会话能力无缝嵌入外部宿主。

---

## 🚀 核心架构与特性

- **零端口冲突与天然绑定**：纯 STDIO 管道通信，不监听任何 TCP/HTTP 端口，无端口占用或防火墙拦截风险；当 Electron/父进程退出或崩溃时，STDIN 管道自动触发 EOF，`thunder-daemon` 毫秒级优雅退出，杜绝后台僵尸进程。
- **并发控制信号量与背压调度**：内置异步任务信号量（`THUNDER_MAX_CONCURRENT_TASKS` 环境变量可调，默认 `8`），限制并发重度推理任务数量，防止本地资源与模型并发配额耗尽。
- **异步 STDOUT Actor（消息零交错）**：所有事件流推送统一通过专用 STDOUT Actor 与带缓冲的 MPSC Channel 排队发送。在高并发多任务流式输出时，**确保每一行 NDJSON 绝对完整且不出现字符交错**。
- **权威历史重载与分层轨迹保留**：任务启动前自动从磁盘存储重载最新权威会话历史，保证多端或重连时消息不错乱；内存轨迹采用分层保留策略——高频微增量（`TokenDelta` / `ReasoningDelta` / `ToolCallChunk`）实时推送至 STDOUT 但不落入内存轨迹，杜绝常驻守护进程内存无限增长。
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
```

### 2. 守护进程输出（STDOUT）

- **请求响应**：`{"type":"response","id":"req-1","success":true,"data":{...}}`
- **流式增量事件**：`{"type":"observed_event","task_id":"task-1","event":{...}}`
- **反问挂起（需用户回答）**：`{"type":"user_question","task_id":"task-1","question_id":"task-1:q1","questions":[{"question":"...","options":[{"label":"..."}]}]}`
- **任务已暂停**：`{"type":"task_paused","task_id":"task-1","reason":"..."}`
- **任务成功完成**：`{"type":"task_completed","task_id":"task-1","session_id":"...","final_content":"...","finish_reason":"Done"}`
- **任务失败**：`{"type":"task_failed","task_id":"task-1","error":"..."}`

### 3. 通用弹窗子协议（ui_request / answer_ui）

与 `ask_user_question` 并列的第二类人机交互，语义对齐 pi 的 `extension_ui_request`。
**只传数据，不传组件** —— 同一套协议可被 TUI、Electron 面板或无头宿主复用。

守护进程 → 宿主（阻塞，直到 `answer_ui` 抵达或 `timeout_ms` 超时）：

```json
{"type":"ui_request","request_id":"ui_1234_1_99","task_id":"task-1","session_id":"sess-001",
 "source":"host","ui":"select","title":"Run rm -rf build/","options":["Allow once","Always allow","Deny"],"timeout_ms":60000}
{"type":"ui_request","request_id":"ui_1234_2_100","source":"host","ui":"confirm","title":"Proceed?","message":"将写入 3 个文件"}
{"type":"ui_request","request_id":"...","source":"host","ui":"input","title":"输入值","placeholder":"..."}
{"type":"ui_request","request_id":"...","source":"host","ui":"editor","title":"编辑","prefill":"..."}
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

请求 id 由守护进程签发（进程号 + 计数器 + 随机后缀），插件无法自选，因此无法
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

`permission`（能力档位）与 `mode`（审批策略）是**完全正交**的两层：

- **档位 = 能力天花板**。`read` / `write` / `bash`，决定"什么根本不可能"。
  由宿主注册哪些工具 + `PermissionGuardMiddleware` 强制执行。daemon 目前固定使用 `bash`（完整能力），不再有角色层来压低档位。
- **模式 = 什么时候向人类确认**。纯粹关于审批交互策略，绝不篡改或下压档位。

### 四种纯净模式

| `mode` | 读 (`Read`) | 写 (`Write`) | 执行 shell (`Exec`) | 说明 |
|--------|:---:|:---:|:---:|------|
| **`never`** (或 `yolo`) | 允许 | 允许 | 允许 | **全自动执行**：完全不弹窗，信任模型自主操作（默认值） |
| **`shell_only`** (或 `accept_edits`) | 允许 | 允许 | **询问** | **自主编写，受控执行**：放行写文件，Shell 命令弹窗询问 |
| **`mutations`** (或 `ask`) | 允许 | **询问** | **询问** | **标准安全防御**：读代码免问，改写文件与 Shell 均弹窗询问 |
| **`always`** (或 `manual`) | **询问** | **询问** | **询问** | **全量审计**：调用任何工具均需每步确认 |

### 三条硬规则

1. **审批模式永远不影响权限天花板。** 模式只决定是否弹窗，不能越权抬升，也不会下压档位。
   `{"permission":"read","mode":"never"}` 依然是绝对只读 —— 物理不可越权。
2. **`never` 只是"不问"，不是"给权限"。** daemon 默认即完整权限（`bash`）。
3. **询问是 fail-closed 的。** 无面板 / 超时 / 用户关闭 → 一律按**拒绝**处理。
   拒绝会作为**工具结果**回灌给模型（附带"什么都没发生"的 ground truth），
   而不是抛异常，避免模型以为成功而反复重试。

### 默认值是 `yolo`（即"不询问"）

审批门是 **opt-in** 的。原因很实际：门的语义是 fail-closed，
无面板宿主上每个弹窗都会被判为"取消"＝"拒绝"。若默认 `ask`，
升级后所有无面板用户的**每一次写文件、每一条 shell、每一个插件工具**都会被静默拒绝。
所以默认值保持不询问；daemon 不再提供角色层来改写模式。

### 交互弹窗长这样

```json
{"type":"ui_request","request_id":"ui_...","source":"host","ui":"select",
 "title":"Run bash","options":["Allow once","Always allow","Deny","Deny with reason"],
 "detail":"rm -rf build/"}
```

- 弹窗**串行**投递：同一批并行工具调用不会叠出多个框，避免批准错对象。
- `Allow once` 只对**当前这一次调用**生效；批准结果绑定 `(tool_call_id, 参数哈希)`，
  参数变了就失效，无法重放。
- `Always allow` 只在能推导出**窄规则**时出现（如 `git status`、`src/lib.rs`），
  且规则对 shell 命令做**链接符过滤**：`git status` 永远不会授权
  `git status && rm -rf /`。插件类工具（参数无可用作用域）**不提供**该选项。
- `Deny with reason` 会追问一句，原因原文回灌给模型。

### 在哪里生效

判定只有**一个**入口：`SessionPolicy::decide`。它外面套的每一层都只负责照做。

```text
PermissionGuardMiddleware   唯一判定 + 执行 —— 决定 Allow/Deny/Ask，然后照做
  └─ SecurityGuard / ResourceGuard / Transaction / …
```

这一层的位置是承重的，两条都有测试钉住：

- **档位被拒的调用根本走不到弹窗** —— 用户无法"批准"绕过档位；
- **被拒的调用不会创建任何临时文件** —— 拒绝发生在事务层之外。

判定顺序本身就是安全属性，同样有测试：**先查档位，再查已记住的规则，最后才看模式**。
"Always allow"是一次同意的记录，而同意不能扛过天花板被收紧 —— 否则在一个只读 run 里
`bash(git status)` 的旧规则会继续生效。

### 插件也是同一条路径

TypeScript 插件的 `ctx.exec()` / `ctx.fs.*` 不再自己实现 shell 与写入，而是合成为
`bash` / `write_file` / `read_file` 工具调用后走**同一条 pipeline**。因此：

- 插件的调用受**模式**与**已记住的规则**约束，和模型调用一致；
- 受禁用命令表、路径 jail、事务层约束；
- 需要审批时，弹窗显示请求方插件名。

代价：插件的写入现在会走事务层（产生 `.arp/tmp` 影子文件、保留原文件权限），
比之前的手写版本更严格 —— 与模型的 `write_file` 一直以来的行为一致。

副作用：`mode: ask` 现在真的管住插件了。这正是这次修复的目的。

### 权限判定只有一张表

工具名 → 能力只有一张表（`ToolEffect::of`）。此前有两张，且不一致：
`Permission::allows_builtin` 对不认识的名字返回 `true`，于是 `apply_patch`
（会改文件、不在该表里）在一个只读 run 下被放行。现在未分类的工具按
"需要写权限"处理，不会仅因为"没人认识"就拿到 shell 权限。

`get_permission_state` 可查看当前模式与已记住的规则。

---

## 🔒 权限档位

档位与 `mode` 的组合效果见上面的「审批模式」。下面是档位本身：

| 档位 | `read_file` | `write_file` | `bash` | 说明 |
| :---: | :---: | :---: | :---: | :--- |
| **`read`** | ✅ | ❌ | ❌ | 只读审查模式；写文件与命令执行工具不注册，TS 插件写操作同样被阻断 |
| **`write`** | ✅ | ✅ | ❌ | 代码修改模式；允许读写文件，禁止执行任意终端命令 |
| **`bash`** (默认) | ✅ | ✅ | ✅ | 完全授权模式；开放所有内置与系统工具 |

---

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

---

## 📄 开源协议

本项目采用 [Apache License 2.0](../LICENSE) 开源许可证。
