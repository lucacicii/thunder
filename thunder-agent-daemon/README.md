# Thunder Daemon (Sidecar for Electron & Desktop UI)

`thunder-daemon` 是专为 Electron、桌面应用及外部宿主设计的轻量级 **STDIO Sidecar 守护进程**。

## 特性

- **零端口冲突**：使用标准输入输出（STDIO）进行 NDJSON（Newline Delimited JSON）行传输，不监听网络端口，无防火墙拦截风险。
- **生命周期天然绑定**：当宿主（Electron 主进程）退出或崩溃时，STDIN 管道自动关闭（EOF），`thunder-daemon` 会干净退出，杜绝后台僵尸进程。
- **全套引擎能力**：内置 `ThunderRoot` 微内核，动态调度 `ConversationPlugin`、`SkillsPlugin`、`McpPlugin`。
- **实时流式推送**：原生透传 `ObservedEvent`（包括 `TokenDelta` 打字机 Token、工具执行中/完成状态、Turn 切换）。
- **任务级取消机制**：支持随时通过 `cancel_task` 中断正在执行的长任务。
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
{"method":"run_task","id":"req-3","task_id":"task-1","prompt":"帮我列出当前目录下的文件","session_id":"sess-001"}
{"method":"cancel_task","id":"req-4","task_id":"task-1"}
```

### 2. 守护进程输出（STDOUT）

- 请求确认：`{"type":"response","id":"req-1","success":true,"data":{...}}`
- 流式事件：`{"type":"observed_event","task_id":"task-1","event":{...}}`
- 任务完成：`{"type":"task_completed","task_id":"task-1","session_id":"...","final_content":"...","finish_reason":"Done"}`
- 任务失败：`{"type":"task_failed","task_id":"task-1","error":"..."}`

日志统一输出到 `STDERR`，绝不污染 `STDOUT` 数据流。
