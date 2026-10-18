# ⚡ Thunder Agent Pack Code

> **Thunder Agent 的编码能力包：shell/文件工具、工作区路径作用域、原子写入、变更上报与工程默认提示词**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-pack-code` 是 `thunder-agent-loop` 的**能力包**。内核是通用的：它只知道流式、工具、上下文裁剪与终止判定，**刻意不知道什么是 shell、工作区或文件**。这些属于这个包——一个*编码*宿主才会装的东西。业务宿主（原型编辑器、表单机器人）可以装别的包，或者什么都不装，上面这些一样都不会跟过来。

---

## 🚀 核心特性

1. **六个内建工具**：`bash`、`read_file`、`write_file`、`grep`、`find`、`ls`。全部 `with_default_cwd(workspace)`，所以调用里的相对路径 = 相对工作区，而不是进程的当前目录。
2. **`CodeScope`：位置，不是裁决**：
   - `classify(target, effect)` 只回答"这次调用落在哪"——`InScope` / `OutOfScope` / `Unknown` / `Critical`。
   - 它**只放宽、从不收紧**：`Unknown` 不赚取自动放行，于是它变成一次询问，而不是一次静默拒绝。
   - `Exec` 与 `Other` 一律 `Unknown`：`rm -rf $BUILD_DIR` 的诚实分类就是"本文件判断不了"。
   - 逐段 canonicalize 解析符号链接，是全仓唯一的逃逸检查点。
3. **原子写 + 进程级 per-path 锁**：
   - 目标同目录建暂存文件 → `write_all` → `sync_all`（先落盘再改名，崩溃后不会留下正确命名的空文件）→ 恢复权限 → `rename`。
   - `FILE_MUTATION_LOCKS` 按**物理路径**（canonicalize 后的路径或"父目录 + 文件名"）排队，相对/绝对/符号链接父目录的不同拼写拿到同一把锁。
   - 取消令牌与写入竞速：被取消的 run 让目标要么原样、要么完整写入。
   - 锁是**进程内**的：两个 daemon、或 daemon 与 TUI 之间不通过它协调。
4. **变更上报**：
   - `write_file` 每次写后上报；`bash` 在命令前后各取一次 `git status --porcelain` 快照并差分——shell 的文件副作用从参数看不出来，这是唯一仍需要快照的地方。
   - 产出内核不认识的自定义事件 `file_change`（`FILE_CHANGE` 常量，**不要改名**，它是 wire 契约）。
5. **工程默认提示词**：`DEFAULT_AUTONOMOUS_SYSTEM_PROMPT`——"先理解再动手"，计划本身不是提示词要求的阶段，而是**用户控制的机制**；另有一段 `# Presentation`，把回答形态绑定到实际渲染器能画的东西（表格 + 带语言围栏）。图表规则不写死在提示词里：`autonomous_system_prompt(RenderCapabilities)` 按宿主能力装配，TUI 拿到的版本不承诺 mermaid，桌面可视化聊天才追加。

---

## 🛠️ 快速上手

### 1. 现代插件化装配（推荐）

六个编码工具已全面实现 MCP 对齐的 `ToolPlugin` 契约，可通过 `code_plugins(...)` 获取插件列表：

```rust
use thunder_agent_loop::prelude::*;
use thunder_agent_pack_code::code_plugins;

let ws = Some(std::path::PathBuf::from("/path/to/project"));
for plugin in code_plugins(ws) {
    agent.register_plugin(plugin);
}
```

### 2. 能力包整体装配

```rust
use thunder_agent_loop::prelude::*;
use thunder_agent_pack_code::{install_code_pack, CodePackConfig};

let config = CodePackConfig {
    register_builtins: true,
    apply_default_prompt: false,
};

// 装上 scope + 六个内建工具；每个工具都拿到 workspace 作为默认 cwd。
let agent = install_code_pack(agent, &config);
```

只想单独用某一块时，各部件都是公开的：

```rust
use thunder_agent_pack_code::{builtin::WriteFileTool, CodeScope};

let agent = agent.with_tool_scope(std::sync::Arc::new(
    CodeScope::from_agent_config(&config, workspace.as_deref()),
));
agent.register_tool(std::sync::Arc::new(
    WriteFileTool::default().with_default_cwd(ws.clone()),
));
```

---

## 🧰 工具清单

| 工具 | 参数 | 要点 |
|---|---|---|
| `bash` | `command`, `cwd?`, `timeout_ms?` | 默认超时 **120s**；`bash -c`；stdin 置空防交互挂死；环境变量脱敏（键名含 `KEY`/`TOKEN`/`SECRET`/`PASSWORD`/`AUTH`/`CREDENTIAL` 或前缀 `AWS_`/`GITHUB_`/`OPENAI_`/`ANTHROPIC_`/`DEEPSEEK_` 的一律剔除，`THUNDER_ENV_PASSTHROUGH` 可显式放行）；Unix 下 `process_group(0)` + 超时杀整组 |
| `read_file` | `path`, `offset`, `limit` | 单次上限 **10 MB** |
| `write_file` | `path`, `content` | 原子写 + per-path 锁（见上） |
| `grep` | `pattern`, `path`, … | ripgrep 内核**进程内**嵌入（`grep-searcher` + `grep-regex` + `ignore`），不起子进程；上限 100 行匹配 / 单行 500 字符 / 输出 50 KB |
| `find` | `pattern`, `path` | `ignore` crate 遍历（`.gitignore` 感知）；上限 1000 条 / 50 KB |
| `ls` | `path` | 每目录上限 1000 条 |

输出超过阈值时由内核的注册表统一溢写到 scratchpad——这是**每个工具**都有的行为，与装不装本包无关，所以本包不包装注册表。

---

## 📄 已知细节

- `install_code_pack` 在生产路径上**没有调用方**：`thunder-agent-root` 与 TUI 都是自己逐个 `register_tool` 并把 `CodeScope` 单独装上去。这个便捷函数目前只有测试在用。
- `CodePackConfig::apply_default_prompt` 声明了但**从未被读取**；默认提示词实际由宿主显式取用（`DEFAULT_AUTONOMOUS_SYSTEM_PROMPT`）。
- `CodeScope::new` 的文档写 "with the pack's default protections in place"，但构造函数把 `protections` / `write_blocked` 都置空；保护实际由 `from_agent_config` → `with_thunder_data_root` 施加。文档与实现不符。
- `read_file` 的 10 MB 上限只在 `limit` 未指定时生效，且随后仍是整文件读入再切行——"分块"只限制输出，不限制内存。

---

## 📄 开源协议

本项目采用 [Apache License 2.0](../LICENSE) 开源许可证。
