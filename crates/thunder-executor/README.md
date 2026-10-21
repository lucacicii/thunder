# 🛡 thunder-executor

`thunder-executor` 是 Thunder 的进程生命周期监管、沙箱执行与输出防护层。

它负责将异构的本地脚本与长连接 MCP 子进程安全地沙箱化，提供进程组管理、超时熔断、防死锁排空与超长日志落盘保护。

---

## 核心职责

1. **防死锁并发流排空与头尾 40% 截断保护 (`flood_guard.rs`)**：
   * **消除 OS 管道死锁**：在派生子进程后，使用两个独立的异步任务持续消费 `stdout` 与 `stderr` 直至 EOF，避免输出超 64KB 导致子进程阻塞在 `write(2)`。
   * **头尾 40% 截断**：内存中保留前 40% 与后 40%（严格按 UTF-8 字符边界通过 `safe_slice_to` / `safe_slice_from` 对齐），防止末尾报错堆栈被丢弃。
   * **溢出日志落盘**：超过 64KB 阈值的数据持续追加写入 `/tmp/thunder_output_*.log`。

2. **独立进程组沙箱 (`script_runner.rs`)**：
   * 通过 `cmd.process_group(0)` 将子进程放入独立进程组。
   * **超时强杀与僵尸进程回收**：超时时向 `-pid` 发送 `SIGKILL` 级联杀灭所有子孙进程，并调用 `child.wait().await` 回收描述符。
   * **参数与环境变量**：通过 `stdin` 发送 JSON 参数并关闭 EOF，注入 `THUNDER_CWD`、`THUNDER_SESSION_ID`，重定向 cwd。
   * 提供 `execute_isolated_script`、`execute_shell_command` 及 `ScriptToolProvider`。

3. **ANSI 终端控制码剥离 (`ansi.rs`)**：
   * 在工具非零退出码返回错误时，自动利用正则剥离 ANSI 颜色代码与光标控制符，向 LLM 提供干净的可读报错。

4. **长连接 Stdio MCP 客户端池 (`mcp_client.rs`)**：
   * `StdioMcpClient`：持久化管理子进程标准输入输出的 JSON-RPC 2.0 长连接。
   * **单帧上限保护**：按行读取时单帧上限严格限制为 4 MiB（`DEFAULT_MAX_FRAME_BYTES`），超限自动丢弃防止 OOM。
   * **RAII 请求清理**：通过 `PendingEntry` 保证请求超时或断连时立即清理待回复表，不发生内存泄漏。
   * `McpToolProvider`：实现 `ToolProvider`，每次调用自动注入 `_meta` 上下文信封，并根据 Schema 自动将第三方 MCP 的相对路径参数转换为基于 `workspace_cwd` 的绝对路径。

5. **结构化超长输出落盘 (`scratchpad.rs`)**：
   * `ScratchpadManager`：统一将超长工具输出持久化到 `<workspace>/.thunder/scratchpad/`，记录 `ArtifactManifest` 清单并返回结构化文件句柄引导模型按行分页查阅。
