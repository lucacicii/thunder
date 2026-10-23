# 🔌 thunder-daemon

`thunder-daemon` 是 Thunder 的 STDIO Sidecar 守护进程（bin: `thunder-daemon`），为 Electron 桌面应用（如 `agent-resume-panel`）与无头宿主提供标准化接口。

它在标准输入输出上运行 **NDJSON 协议（Protocol v3）**，负责任务调度、流式事件转发与多工作区隔离。

---

## 协议与功能特性

1. **基础指令**：
   * `ping`：心跳检测，返回 `status: "pong"`, `protocol_version: 4`。
   * `list_models`：读取 `thunder-providers` 目录并上报可用模型。
   * `list_conversations` / `get_conversation`：读取持久化的会话。
   * `set_conversation_title` / `generate_title`：手动设置或 AI 生成会话标题。
   * `reload_integrations`：重载本地配置。
   * **v4 删除**：`answer_ui`（没有待答复的对话框，它只会对空无回 `delivered: true`）、
     `get_trace` / `list_traces`（trace 存储随插件框架一起没了，永远回 `traces: []`），
     以及从未发出过的 `suggested_questions` 帧。

2. **任务与运行控制**：
   * `run_task`：启动 Agent 任务，实时向客户端推送 `observed_event` 帧，并在结束时发出 `task_completed` 或 `task_failed`。
   * `pause_task` / `resume_task`：在工具边界协作式暂停 / 恢复任务（推送 `task_paused`）。
   * `steer_task` / `clear_queue`：在任务执行过程中实时插入纠偏指令（`steer`）或后续指令（`follow_up`），推送 `task_queue_update`。
   * `cancel_task`：优雅取消任务。

3. **权限与模式管理**：
   * `get_permission_state` / `set_mode`：管理会话模式（`yolo` / `plan`；`ask` 等旧名一律归一为 `yolo`，因为 daemon 只对 `plan` 有分支），状态变动时推送 `mode_changed` 帧。
   * 集成 `thunder-plan`，在 `plan` 模式下拦截文件修改，并在 `exit_plan_mode` 成功后自动将模式翻转回 `yolo`。

4. **人机互动（问答气泡）**：
   * 集成 `thunder-ask`：当模型调用 `core__ask_user_question` 时，向桌面端推送 `user_question` 气泡；
   * 客户端发送 `answer_question` 指令后，精确唤醒被挂起的目标任务。
   * `headless: true` 的 run **不注册**这个工具：没有界面可以答复时，让它出现在
     工具目录里只会让模型白问一轮。

---

## 启动方式

```bash
# 通过配套脚本启动
./daemon.sh

# 或直接运行二进制
cargo run -p thunder-daemon --bin thunder-daemon

# 指定工作区与独立数据根
cargo run -p thunder-daemon --bin thunder-daemon -- --workspace /path/to/project --config-dir /tmp/thunder-dev
```

`--config-dir` 等价于先导出 `THUNDER_CONFIG_DIR`，会把 `conversations/`、`state/`、`scratchpad/`、`logs/` 以及 `models.json` / `auth.json` 的查找根整体重定向。

---

## 运行时状态与热重启（SIGUSR1）

启动时 daemon 会写一份运行时标记：

```jsonc
// <data_root>/state/daemon-<pid>.json
{
  "pid": 86390,
  "exe": "/Users/you/.cargo/bin/thunder-daemon",
  "version": "0.1.0",
  "protocol_version": 4,
  "hot_reload": true,
  "started_at_ms": 1792671257555,
  "config_dir": "/Users/you/.thunder"
}
```

`hot_reload: true` 表示这个 daemon 支持**原地换心**。收到 `SIGUSR1` 时：

| 条件 | 行为 |
|---|---|
| `active_tasks` 非空 | 拒绝，日志 `warn`，旧进程继续服务 |
| `current_exe` 文件缺失 / 无执行位 | 拒绝，日志 `error`，不冒险 exec |
| 空闲且二进制可执行 | 等 120ms 冲刷在途 NDJSON 帧 → `execv(自己, 原 argv)` |

`execv` 保留 pid、argv、环境变量与 stdio 管道，所以 Electron 桌面端的 readline 连接不会断开，无需重启 App。新进程重新写一份状态文件（`started_at_ms` 变化），调用方据此确认成功。

调用入口是 `thunderctl reload`（见 [`crates/thunder-cli`](../thunder-cli/README.md)）。**不要手工 `kill -USR1`**：对不认识该信号的旧版本，它的默认动作是终止进程。
