# ⚡ Thunder Agent Plugin

> **High-Performance Host Engine & Sandbox for Single-File TypeScript Plugins**

[English](README_en.md) | [简体中文](README.md)

`thunder-agent-plugin` is the dynamic plugin runtime for Thunder. It allows developers to author custom tools and plugins in single-file TypeScript, executed natively by a Node.js sidecar (`runner/host.mjs`) without pre-compilation, featuring blue-green zero-downtime hot reloading and syntax error immunity.

---

## 🚀 Key Features

- **Native TypeScript Execution (Zero Pre-compilation)**: Leverages Node.js 20+ `node --experimental-strip-types` to execute `.ts` plugin files directly without requiring a separate compilation step.
- **Blue-Green Hot Reloading**: Watches `.arp/plugins/*.ts` for file updates. The sidecar compiles and validates the updated module in background before swapping active instances seamlessly without interrupting in-flight tasks.
- **Syntax Error Immunity**: If a newly saved TypeScript file contains syntax errors or runtime exceptions on initialization, the sidecar catches the error, logs a diagnostic warning, and **retains the previous healthy version of the plugin**, preventing host crashes.
- **Role Permission Enforcement**: Plugin capabilities in `PluginContext` are strictly bound to the active role's permission tier:
  - In read-only mode (`Permission::Read`), calls to `ctx.fs.writeFile()` or `ctx.exec()` are rejected immediately, preventing plugins from acting as permission bypasses.
- **Zero-Overhead Tool Bridging**: Functions exported from TypeScript plugins are dynamically transformed into Rust `AgentTool` instances consumable by `thunder-agent-loop`.

---

## 🛠️ Writing a TypeScript Plugin

Create a plugin at `.arp/plugins/calc_plugin.ts` in your workspace:

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
        const result = Function(`"use strict"; return (${args.expression})`)();
        return `Result: ${result}`;
      },
    },
  ],
};
```

---

## 📡 Sidecar Architecture & IPC

The Rust host maintains a persistent connection with `runner/host.mjs` over asynchronous Stdio using NDJSON:
- `manifest`: Sidecar starts and synchronizes the active plugin roster and tool JSON schemas.
- `execute_tool`: Rust dispatches tool execution requests; sidecar responds asynchronously with results or errors.
- `reload`: File modifications trigger hot-reload and schema updates.

---

## 📄 License

This project is licensed under the [Apache License 2.0](../LICENSE).
