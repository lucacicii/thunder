# ⚡ Thunder Agent Plugin

> **面向 TypeScript 单文件插件的高性能宿主引擎与安全沙箱**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-plugin` 是 Thunder 体系的动态插件宿主。它允许开发者直接使用 TypeScript 编写单文件插件，由底层的 Node.js Sidecar（`runner/host.mjs`）原生加载执行，无需预编译步骤，并具备蓝绿热重载与语法错误免疫机制。

---

## 🚀 核心架构与特性

- **原生 TypeScript 执行（零预编译）**：利用 Node.js 20+ 的 `node --experimental-strip-types` 特性，直接加载执行 `.ts` 插件源文件，省去繁琐的构建打包流程。
- **蓝绿无缝热重载（Blue-Green Hot Reload）**：内置文件变更监听器。当修改 `.arp/plugins/*.ts` 文件时，Sidecar 在后台完成新版本编译验证后无缝热切换，不中断运行中的 Agent 任务。
- **语法错误免疫（Error Immunity）**：若新保存的 TypeScript 文件存在语法错误或初始化异常，Sidecar 会自动捕获并在日志中告警，同时**继续保留并运行上一个健康的插件版本**，确保宿主系统绝不崩溃。
- **角色权限对齐（Permission Enforcement）**：插件上下文（`PluginContext`）提供的能力与当前任务的角色权限档位严格绑定：
  - 在只读角色（`Permission::Read`）下，插件调用 `ctx.fs.writeFile()` 或 `ctx.exec()` 会被直接拦截拒绝，杜绝插件成为越权旁路。
- **无缝 AgentTool 桥接**：TypeScript 插件中导出的函数自动转换为 Rust 端的 `AgentTool`，无缝供 `thunder-agent-loop` 调度调用。

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

## 📄 开源协议

本项目采用 [Apache License 2.0](../LICENSE) 开源许可证。
