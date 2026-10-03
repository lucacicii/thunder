# ⚡ Thunder Agent Plugin

> **面向 TypeScript 单文件插件的高性能宿主引擎与安全沙箱**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-plugin` 是 Thunder 体系的动态插件宿主。它允许开发者直接使用 TypeScript 编写单文件插件，由底层的 Node.js Sidecar（`runner/host.mjs`）原生加载执行，无需预编译步骤，并具备蓝绿热重载与语法错误免疫机制。

---

## 🚀 核心架构与特性

- **原生 TypeScript 执行（零预编译）**：利用 Node.js 20+ 的 `node --experimental-strip-types` 特性，直接加载执行 `.ts` 插件源文件，省去繁琐的构建打包流程。
- **蓝绿无缝热重载（Blue-Green Hot Reload）**：内置文件变更监听器。当修改 `.arp/plugins/*.ts` 文件时，Sidecar 在后台完成新版本编译验证后无缝热切换，不中断运行中的 Agent 任务。
- **语法错误免疫（Error Immunity）**：若新保存的 TypeScript 文件存在语法错误或初始化异常，Sidecar 会自动捕获并在日志中告警，同时**继续保留并运行上一个健康的插件版本**，确保宿主系统绝不崩溃。
- **权限档位对齐（Permission Enforcement）**：插件上下文（`PluginContext`）提供的能力与当前任务的权限档位严格绑定：
  - 在只读档位（`Permission::Read`）下，插件调用 `ctx.fs.writeFile()` 或 `ctx.exec()` 会被直接拦截拒绝，杜绝插件成为越权旁路。
- **无缝 AgentTool 桥接**：TypeScript 插件中导出的函数自动转换为 Rust 端的 `AgentTool`，无缝供 `thunder-agent-loop` 调度调用。
- **宿主能力直通**：`ctx.ui`（select / confirm / input / editor / notify / setStatus）向宿主请求弹窗；`ctx.callTool` 调用宿主已注册的工具。二者都**经过宿主代理**，不绕过任何策略层（详见下方「安全边界」）。

---

## 🛠️ TypeScript 插件编写范例

在工作区目录的 `.arp/plugins/calc_plugin.ts` 中创建插件：

```typescript
export default {
  name: "calc_plugin",
  description: "High-speed custom calculator tool",
  tools: [
    {
      name: "evaluate_math",
      description: "Safely evaluate mathematical expressions",
      parameters: {
        type: "object",
        properties: {
          expression: { type: "string", description: "Arithmetic formula, e.g. 24 * 7" },
        },
        required: ["expression"],
      },
      execute: async (args: { expression: string }, ctx: any) => {
        // 执行安全计算
        const result = Function(`"use strict"; return (${args.expression})`)();
        return `Result: ${result}`;
      },
    },
  ],
};
```

---

## 📡 进程间通信与 Sidecar 机制

Rust 宿主通过异步 Stdio 与 `runner/host.mjs` 维持长连接，通信采用标准 NDJSON 协议：
- `manifest`：Sidecar 启动并向 Rust 同步已发现的插件清单与工具签名。
- `execute_tool`：Rust 端派发工具调用，Sidecar 执行后异步回传结果或错误。
- `reload`：文件修改后触发自动重新加载与清单增量同步。

---

## 🔐 安全边界

插件层是体系里护栏最少的一层，因此所有特权都必须由宿主代理，插件不得绕过。

| 能力 | 走哪条路 | 约束 |
|------|---------|------|
| `ctx.exec()` / `ctx.fs.*` | 侧车 → Rust RPC → **完整 onion** | 合成为 `bash` / `write_file` / `read_file` 工具调用后派发，与模型调用**同一条 pipeline**：能力档位、模式、已记住的规则、路径 jail、事务、禁用命令表、审批门全部生效 |
| `ctx.ui.*` | 侧车 → Rust → 客户端 | 线上恒为 `source: "plugin"`，客户端**不得**把插件弹窗的答复当作授权决定 |
| `ctx.callTool()` | 侧车 → Rust → **完整 onion** | 同上；被询问时弹窗会显示请求方插件名 |
| 审批 | `ApprovalGate`（`PermissionGuard` 之内） | 一律 fail-closed：无面板 / 超时 / 用户拒绝 → 拒绝，并以「什么都没发生」的 ground truth 回灌模型 |

`pluginId` 由 Node 侧随请求带上：侧车是多插件共用一条通道，只有它知道是谁在问。
**没有归属信息的插件调用与模型调用无法区分**，那等于给了一个可以伪装成助手的提权通道。

### 为什么 `ctx.exec` 不再自己实现

早期版本里 `ctx.exec()` 是手写 `bash -c`、`ctx.fs.writeFile()` 是手写原子写 —— 也就是
把工具语义实现了第二遍。后果不是"重复代码"，而是**两份实现不一致**：

- `SecurityGuard` 的禁用命令表不生效，插件能跑模型跑不了的命令；
- 档位是唯一的检查，审批模式与"总是允许"规则都不生效 ——
  `mode: ask` 下模型发 `bash` 会被问，插件发 `ctx.exec("git status")` 不会；
- 路径 jail 与 10MB 上限各写一份，靠人工保持一致。

现在这三个 RPC 只是**表达成它一直伪装成的那个工具调用**，交给 run 的 invoker 派发。
参数名本来就与工具 schema 一致（`bash` 收 `{command, cwd}`，`ctx.exec` 收
`(command, {cwd})`），所以映射是恒等的，并且逐方法显式写出：schema 变了会在映射处
编译失败，而不是运行时静默错位。

run 尚未注册工具时（`on_run_ready` 之前），`ctx.exec` 会被拒绝并说明原因 ——
**不再有第二条静默的旁路**。

### 并发隔离

侧车是**一个** Node 进程，服务所有并发 run。因此它自身不持有任何能力：能力按
「本次调用属于哪个 run」查表获得（`RunRegistry`，键是 tool call 上的 `route`）。

| 服务 | 为什么必须按 run 隔离 |
|------|---------------------|
| 能力档位 | 否则先启动的 run 会被后启动的 run 改写权限 |
| workspace root | invoker 就是**整条 pipeline**，带着该 run 的 workspace 与路径 jail —— 串线就是跨 workspace 写入 |
| 宿主 UI | 否则弹窗会被路由到另一个任务 |
| tool invoker | 否则 A 的插件调用会派发进 B 的 pipeline |

因此每个反向 RPC 都必须带 `route`。**没有 `route`、或 `route` 指向已结束的 run，一律拒绝**，
绝不回退到「某个 run 的权限」。

两阶段握手：宿主先派发 `on_init`（此时 agent 尚未存在，只能给出身份与策略），
待所有工具注册完成、pipeline 定型后再派发 `on_run_ready`（此时才能交出 invoker）。
注册表有容量上限，宿主崩溃导致 `on_finish` 未执行时按先进先出淘汰 —— 淘汰的后果是**拒绝**，绝不是放宽。

`route` 由宿主在每次 `execute()` 时生成（daemon 传 `task_id`，便于面板日志对照）；
同一 session 的多个 run 也不会重号。

## 📄 开源协议

本项目采用 [Apache License 2.0](../LICENSE) 开源许可证。
