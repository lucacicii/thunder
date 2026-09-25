# Thunder Daemon (Sidecar for Electron & Desktop UI)

`thunder-daemon` 是专为 Electron、桌面应用及外部宿主设计的轻量级 **STDIO Sidecar 守护进程**。

## 特性

- **零端口冲突**：使用标准输入输出（STDIO）进行 NDJSON（Newline Delimited JSON）行传输，不监听网络端口，无防火墙拦截风险。
- **生命周期天然绑定**：当宿主（Electron 主进程）退出或崩溃时，STDIN 管道自动关闭（EOF），`thunder-daemon` 会干净退出，杜绝后台僵尸进程。
- **全套引擎能力**：内置 `ThunderRoot` 微内核，动态调度 `ConversationPlugin`、`SkillsPlugin`、`McpPlugin`。
- **实时流式推送**：原生透传 `ObservedEvent`（包括 `TokenDelta` 打字机 Token、工具执行中/完成状态、Turn 切换）。
- **任务级取消机制**：支持随时通过 `cancel_task` 中断正在执行的长任务。
- **角色与权限（Role / Permission）**：按 `role` 激活 `~/.thunder/roles.jsonl` 中定义的角色；
  角色决定**注册哪些内置工具**（`read` / `write` / `bash`），被禁的工具模型根本看不到。
- **协作式暂停**：`pause_task` 在下一个工具边界冻结任务，`resume_task` 继续；
  不打断进行中的工具，也不销毁会话。
- **反问气泡（ask_user_question）**：agent 可阻塞提问，面板渲染为气泡；
  回答后任务自动继续。
- **内置 Mock 降级**：本地调试未配置 API Key 时可一键降级为 Mock 模式，方便 UI 联调。

## 启动与调试

```bash
# 启动 daemon
./daemon.sh

# 发送测试 Ping
echo '{"method":"ping","id":"1"}' | ./daemon.sh

# 查询可用模型列表
echo '{"method":"list_models","id":"2"}' | ./daemon.sh
```

## 协议说明

### 1. 宿主指令（STDIN）

每行一条 JSON：

```json
{"method":"ping","id":"req-1"}
{"method":"list_models","id":"req-2"}
{"method":"list_roles","id":"req-3","workspace_dir":"/path/to/repo"}
{"method":"run_task","id":"req-4","task_id":"task-1","prompt":"帮我列出当前目录下的文件","session_id":"sess-001","role":"plan"}
{"method":"cancel_task","id":"req-5","task_id":"task-1"}
{"method":"pause_task","id":"req-6","task_id":"task-1"}
{"method":"resume_task","id":"req-7","task_id":"task-1"}
{"method":"answer_question","id":"req-8","question_id":"task-1:q1","answers":{"要重构哪一层？":"渲染层"}}
```

### 2. 守护进程输出（STDOUT）

- 请求确认：`{"type":"response","id":"req-1","success":true,"data":{...}}`
- 流式事件：`{"type":"observed_event","task_id":"task-1","event":{...}}`
- 任务完成：`{"type":"task_completed","task_id":"task-1","session_id":"...","final_content":"...","finish_reason":"Done"}`
- 任务失败：`{"type":"task_failed","task_id":"task-1","error":"..."}`
- **反问（需回答）**：`{"type":"user_question","task_id":"task-1","question_id":"task-1:q1","questions":[{"question":"...","options":[{"label":"..."}]}]}`
- **已暂停**：`{"type":"task_paused","task_id":"task-1","reason":"..."}`

### 角色与权限

`run_task` 的 `role` 字段按 id（或别名）解析，作用域同 `~/.thunder/roles.jsonl` 与
`<workspace>/.arp/roles.jsonl`。省略 `role` 时行为与历史一致（完全放开）。

角色的 `permission` 决定内置工具是否注册：

| 档位 | `read_file` | `write_file` | `bash` |
| --- | :---: | :---: | :---: |
| `read` | ✅ | ❌ | ❌ |
| `write` | ✅ | ✅ | ❌ |
| `bash`（默认） | ✅ | ✅ | ✅ |

只读 role 下，TypeScript 插件的 `ctx.fs.writeFile()` / `ctx.exec()` 也会被拒——
否则插件将成为绕过权限的旁路。

### 暂停语义

`pause_task` 是**协作式**的：在**下一个工具边界**生效，进行中的工具会跑完，
避免半途打断文件写入。`cancel_task` 仍然是不可逆的。

### 反问流程

1. agent 调用 `ask_user_question` → daemon 推 `user_question` 并**挂起**该任务。
2. 宿主回答 `answer_question`（`question_id` 全局唯一，形如 `task-1:q1`）。
3. 工具的返回值即答案文本，loop 继续。

超时（默认 30 分钟）或 `cancel_task` 都会释放挂起的提问。

### 状态机与交叉行为规范（Pause 与 UserQuestion）

任务运行状态机如下：

```text
               ┌──────────┐
               │ Running  │
               └────┬─────┘
          pause_task│   ▲ resume_task
          (工具边界) │   │
                    ▼   │
               ┌──────────┐
               │  Paused  │
               └──────────┘
                    ▲
                    │ 答复完成且曾收到 pause
                    │
         ┌─────────────────────┐
         │ WaitingUserInput    │ ◄── agent 调用 ask_user_question 工具
         │ (user_question 阻塞) │ ──► answer_question 答复后恢复 Running
         └─────────────────────┘
```

1. **生命周期边界定位**：
   - `pause_task`：生效于**工具调度边界（Tool Scheduling Boundary）**。在工具开始执行前检查；若当前已有工具在运行中，该工具会完整跑完，并在下一轮工具调度前进入挂起。
   - `ask_user_question`：属于**工具内部执行（In-Tool Execution）**。它在工具内部等待宿主的 `answer_question` oneshot 信号。
2. **交叉场景处理**：
   - **反问等待中收到 pause**：若任务当前正处于 `user_question` 等待人类输入阶段，`pause_task` 会标记该任务的 PauseGate 为 paused。当前工具（`ask_user_question`）不会被中断，继续等待人类答复。人类通过 `answer_question` 提交回答后，该工具完成返回，任务随之进入下一个工具边界并**立即冻结在 Paused 状态**。
   - **反问等待中调用 resume**：若任务正在等待用户答复，调用 `resume_task` 不会有立竿见影的效果，因为任务是在等待 `answer_question` 响应，而不是在 PauseGate 上阻塞。
   - **Paused 状态下收到 cancel**：无论任务处于 Paused 还是 WaitingUserInput，`cancel_task` 均会立刻唤醒并终止任务。

日志统一输出到 `STDERR`，绝不污染 `STDOUT` 数据流。
