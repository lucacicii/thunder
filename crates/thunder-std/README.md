# 🧰 thunder-std

`thunder-std` 是 Thunder 的核心标准工具包，**以独立可执行二进制形态发布**（bin: `thunder-std`）。

它以 Stdio JSON-RPC 2.0 运行，遵循 MCP 规范，并利用 Thunder 注入的 `_meta` 上下文信封实现跨目录隔离。

---

## 包含的核心工具

| 工具名 | 内部 URI | 说明与专门优化 |
|---|---|---|
| `read_file` | `core://read_file` | 读取文件。**大文件优化**：文件 $\le 10\text{ MB}$ 时全量读取，$> 10\text{ MB}$ 时采用 `tokio::io::BufReader::lines()` 逐行流式读取与分页，防止堆内存 OOM。 |
| `write_file` | `core://write_file` | 写入文件。**原子性优化**：通过 `file_lock_for` 实施基于规范化路径的进程内互斥锁；写入同目录隐藏临时文件，调用 `sync_all` 确保刷盘，再执行原子 `rename`。 |
| `edit_block` | `core://edit_block` | 替换文件文本块。同样受 `file_lock_for` 保护；严格校验 `old_text` 在文件中的唯一匹配性，原子写回。 |
| `bash` | `core://bash` | 执行 Shell 脚本。运行于独立进程组沙箱中，超时发送 `SIGKILL` 级联杀灭所有子孙进程，输出享受 64KB 防暴流截断保护。 |
| `grep` | `core://grep` | 文件内容检索。**内存级 ripgrep 优化**：直接嵌入 `grep-searcher` + `grep-regex` + `ignore`，多线程并行遍历，自动跳过二进制，遵循 `.gitignore`，无进程派生开销。 |
| `find` | `core://find` | 文件路径检索。**内存级 glob 优化**：基于 `ignore` + `globset`，自动遵循 `.gitignore`，匹配达到 1000 条或 50KB 即刻退出遍历。 |
| `ls` | `core://ls` | 单层目录枚举。按目录优先（带 `/`）和文件大小结构化排序。 |

---

## 运行模式

作为标准 MCP Server 运行于标准输入输出：

```bash
# 启动 thunder-std 进程
cargo run -p thunder-std --bin thunder-std
```

优先读取请求中的 `_meta.workspace_cwd` 作为相对路径的基准目录，并使用 `_meta.timeout_ms` 实施超时控制。
