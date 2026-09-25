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
- **角色与权限硬隔离（Role / Permission）**：支持从 `~/.thunder/roles.jsonl` 与工作区 `.arp/roles.jsonl` 加载角色。角色的权限档位（`Read` ⊂ `Write` ⊂ `Bash`）决定内置工具是否注册，被禁工具模型在 Prompt 中**物理不可见**。
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
{"method":"list_roles","id":"req-3","workspace_dir":"/path/to/repo"}
{"method":"run_task","id":"req-4","task_id":"task-1","prompt":"帮我检查当前目录的 git 状态","session_id":"sess-001","role":"plan"}
{"method":"cancel_task","id":"req-5","task_id":"task-1"}
{"method":"pause_task","id":"req-6","task_id":"task-1"}
{"method":"resume_task","id":"req-7","task_id":"task-1"}
{"method":"answer_question","id":"req-8","question_id":"task-1:q1","answers":{"要重构哪一层？":"渲染层"}}
```

### 2. 守护进程输出（STDOUT）

- **请求响应**：`{"type":"response","id":"req-1","success":true,"data":{...}}`
- **流式增量事件**：`{"type":"observed_event","task_id":"task-1","event":{...}}`
- **反问挂起（需用户回答）**：`{"type":"user_question","task_id":"task-1","question_id":"task-1:q1","questions":[{"question":"...","options":[{"label":"..."}]}]}`
- **任务已暂停**：`{"type":"task_paused","task_id":"task-1","reason":"..."}`
- **任务成功完成**：`{"type":"task_completed","task_id":"task-1","session_id":"...","final_content":"...","finish_reason":"Done"}`
- **任务失败**：`{"type":"task_failed","task_id":"task-1","error":"..."}`

---

## 🔒 角色权限档位

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
