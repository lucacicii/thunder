# Thunder

> **极轻量的 Rust AI Agent 内核 + 显式装配的宿主**

[English](README_en.md) | [简体中文](README.md)

Thunder 是一个用 Rust 写的编码 Agent 体系：一个只认两件事的内核（`Tools` 执行工具、`LLMClientTrait` 拿流），外加把产品意见全部拿在手里的宿主。整个生态是单一 Cargo workspace（12 个 crate）。

- 架构与模块地图：[`docs/architecture.md`](docs/architecture.md)
- 权限、审批、四档模式：[`docs/permission-structure.md`](docs/permission-structure.md)
- 0.5 → 0.6 迁移：[`docs/migration-0.6.md`](docs/migration-0.6.md)
- 工作区与开发约定：[`WORKSPACE.md`](WORKSPACE.md)

---

## 核心设计

1. **内核只认接缝**（`thunder-agent-loop`）
   - 工具通过 `Tools::specs()` / `Tools::call()` 提供；模型通过 `LLMClientTrait` 提供。内核不认识 HTTP 方言、不认识 MCP、不认识审批。
   - 取消、超时、配对不变量（每条已宣告的调用都有结果）由内核保证。

2. **宿主显式装配**（`thunder-agent-root`）
   - 一个宿主对一次运行的贡献是四个列表：`PromptSection`（提示词）、`ToolPack`（工具）、`Observer`（事件）、`Prepared`（按运行准备），全部拿到同一个 `RunEnv`。
   - **注册即激活**：没有选择器、没有关键词路由。`.thunder/config.json` 的 `plugins.disabled` 是唯一的减法，所以工具表与提示词前缀不会在轮次之间无声漂移。

3. **单一工具漏斗 + 全量 trace**（`thunder-agent-policy`）
   - 每条调用只走一条路：取消 → 作用域 → 守卫 → 模式裁决 → 弹窗 → 拦截器 → 注册表 → trace。
   - 守卫是地板，任何模式都不能放宽；没有 UI 时弹窗视为拒绝（fail-closed）。细节见 `docs/permission-structure.md`。

4. **字节有界的事件流**（`max_event_bytes`，默认 8 MiB）
   - 队列按字节而不是条数设上限；超限即丢并计数，宿主从 `EventDropCounters` 得知"本次记录可能不完整"。

5. **检查点压缩 + 并发上限**
   - 只在逼近真实窗口时压缩一次；摘要不可用时退化为紧急裁剪；`max_concurrent_summaries`（默认 2）限制共享限流器下的在途摘要数。
   - 两次压缩之间请求前缀字节级稳定，最大化 provider 侧 prompt cache 命中。

6. **进程内模型传输**
   - `thunder-agent-providers` 直接发流式 HTTP：不起子进程、不要求 Node。覆盖 OpenAI chat / OpenAI Responses / Anthropic Messages 等方言，未知方言明确报错。

7. **可选能力包**
   - `thunder-agent-pack-code` 提供 `read_file` / `write_file` / `ls` / `find` / `grep` / `bash`；内核不依赖它。
   - `bash` 的程序由 `ShellConfig` 决定（`$SHELL` 探测或宿主钉死），Unix 下自成进程组、超时杀整组。

8. **多宿主**
   - 终端（`thunder-tui`）、NDJSON sidecar（`thunder-agent-daemon`）、无头 JSONL（`thunder-agent-runtime`）共用同一套装配。

---

## 工作区（12 个 crate）

| Crate | 角色 |
|---|---|
| [`thunder-agent-types`](thunder-agent-types) | 纯 wire：消息 / 事件 / 工具定义与结果 / policy 声明 / scope / trace 记录 |
| [`thunder-agent-loop`](thunder-agent-loop) | 内核：循环、上下文、压缩、流、缓存、`seam` |
| [`thunder-agent-policy`](thunder-agent-policy) | 策略层：工具抽象、执行漏斗、gate、UI、拦截器、只读判据 |
| [`thunder-agent-pack-code`](thunder-agent-pack-code) | 参考能力包：fs / shell / search + `CodeScope` |
| [`thunder-agent-providers`](thunder-agent-providers) | 模型目录与进程内流式传输 |
| [`thunder-agent-skills`](thunder-agent-skills) | Skill 解析、注册与目录注入 |
| [`thunder-agent-mcp`](thunder-agent-mcp) | MCP 客户端 / 传输 / 工具桥接（含帧上限） |
| [`thunder-agent-root`](thunder-agent-root) | 宿主装配（`Assembly` / `StandardHostBuilder`） |
| [`thunder-agent-daemon`](thunder-agent-daemon) | NDJSON sidecar |
| [`thunder-agent-runtime`](thunder-agent-runtime) | JSONL 无头运行时（宿主执行工具） |
| [`thunder-conversation`](thunder-agent-core/conversation) | 会话存储（内存 / 文件系统） |
| [`thunder-tui`](thunder-agent-core/tui) | Ratatui 终端界面 |

---

## 快速开始

要求 Rust 1.80+，不需要 Node.js。

```bash
git clone https://github.com/lucacicii/thunder.git
cd thunder

./run.sh      # 交互式 TUI
./daemon.sh   # NDJSON sidecar
./test.sh     # 全工作区测试
```

嵌入自己的宿主：

```rust
use std::sync::Arc;
use thunder_agent_loop::AgentConfig;
use thunder_agent_root::prelude::*;
use thunder_conversation::prelude::MemoryConversationStore;

let root = StandardHostBuilder::new(Arc::new(MemoryConversationStore::new()))
    .build(ThunderRoot::new(AgentConfig::new("deepseek/deepseek-chat"))
        .with_workspace(std::env::current_dir()?));

let mut handle = root
    .execute("跑一下测试并修好失败的那条", RootRunOptions::default())
    .await?;
if let Some(mut events) = handle.take_events() {
    while let Some(event) = events.recv().await { /* 转发给 UI */ }
}
let result = handle.join().await?;
println!("{:?}", result.final_content);
```

宿主专属能力在 `build` 之后追加：

```rust
root = root.with_pack(ToolPack::new("my_tool").with_tool(Arc::new(MyTool)));
```

---

## 验证

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --features thunder-agent-daemon/testing-mock -- -D warnings
cargo test --workspace
```

CI 跑同一组命令；测试分布见 `docs/architecture.md` §14.4。

---

## 开源协议

[Apache License 2.0](LICENSE)
