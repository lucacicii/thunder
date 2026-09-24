#!/usr/bin/env node
/**
 * Thunder Agent — Lightweight TypeScript & JavaScript Plugin Sidecar Host
 *
 * Zero external npm dependencies.
 * Runs standalone single-file plugins (*.ts / *.js).
 * Supports:
 *   - Capability A: Lifecycle event observation (onEvent)
 *   - Capability B: Context injection (systemPrompt)
 *   - Capability C: Dynamic tools (tools)
 *   - SDK Context: ctx.fs, ctx.exec, ctx.callTool (with cycle guard & max depth 3)
 *   - Blue-Green Hot-Reload: shadow compilation, syntax error immunity, atomic swap
 */

import * as fs from "node:fs";
import * as path from "node:path";
import * as readline from "node:readline";
import * as vm from "node:vm";
import * as nodeModule from "node:module";

// Active plugins registry (Green version)
// Map<pluginId, LoadedPlugin>
const activePlugins = new Map();

// In-flight tool executions counter (for Turn boundary drain)
let inFlightExecutions = 0;
let pendingSwapQueue = [];

// Call depth tracker for ctx.callTool (Cycle Guard)
const MAX_CALL_DEPTH = 3;

/**
 * Global SDK helper available to plugin scripts:
 * export default definePlugin({ ... })
 */
globalThis.definePlugin = function definePlugin(config) {
  return config;
};

// ---------------------------------------------------------------------------
// Protocol Communication with Rust Parent via STDIO (NDJSON)
// ---------------------------------------------------------------------------
const pendingRustRequests = new Map();
let requestIdCounter = 0;

function sendToRust(msg) {
  try {
    process.stdout.write(JSON.stringify(msg) + "\n");
  } catch (err) {
    process.stderr.write(`[ts-host] Failed to write to stdout: ${err.message}\n`);
  }
}

function callRust(method, params) {
  return new Promise((resolve, reject) => {
    const id = `req_${++requestIdCounter}_${Date.now()}`;
    const timer = setTimeout(() => {
      pendingRustRequests.delete(id);
      reject(new Error(`Timeout waiting for Rust response to '${method}' (id=${id})`));
    }, 60000);

    pendingRustRequests.set(id, { resolve, reject, timer });
    sendToRust({
      type: "rpc_request",
      id,
      method,
      params
    });
  });
}

// ---------------------------------------------------------------------------
// Transpiler & Shadow Loader (Blue-Green Hot Reload)
// ---------------------------------------------------------------------------

function transpileTsToJs(sourceCode, filename) {
  let code = sourceCode;
  let strippedNatively = false;

  // 1. Try stripping TS types using Node.js built-in stripTypeScriptTypes if available
  try {
    if (typeof nodeModule.stripTypeScriptTypes === "function") {
      code = nodeModule.stripTypeScriptTypes(sourceCode);
      strippedNatively = true;
    }
  } catch {
    // fallback
  }

  // Remove type-only imports: import type { ... } from '...';
  code = code.replace(/import\s+type\s+[^;]+;/g, "");

  // Remove standard imports: import { ... } from '...';
  code = code.replace(/import\s+[^;]+from\s+['"][^'"]+['"];?/g, "");

  if (!strippedNatively) {
    // Fallback regex type stripping only if native stripTypeScriptTypes was unavailable
    code = code.replace(/\b(export\s+)?(interface|type)\s+[A-Za-z0-9_<>,\s\n\r{}[\]:|&=?]+;?/g, "");
    code = code.replace(/:\s*([A-Za-z0-9_<>[\]]+(\s*\|\s*[A-Za-z0-9_<>[\]]+)*)(\s*[,=)])/g, "$3");
  }

  // Transform export default definePlugin(...) to global export collector
  code = code.replace(/export\s+default\s+definePlugin\s*\(/g, "globalThis.__lastDefinedPlugin = definePlugin(");
  code = code.replace(/export\s+default\s+/g, "globalThis.__lastDefinedPlugin = ");

  return code;
}

/**
 * Load and evaluate a plugin in an isolated shadow scope.
 * Returns { success: true, plugin } or { success: false, error }.
 */
function shadowEvaluatePlugin(filePath) {
  try {
    const rawContent = fs.readFileSync(filePath, "utf-8");
    const jsCode = transpileTsToJs(rawContent, filePath);

    const pluginId = path.basename(filePath, path.extname(filePath));
    const sandbox = {
      console: {
        log: (...args) => process.stderr.write(`[plugin:${pluginId}] ${args.join(" ")}\n`),
        info: (...args) => process.stderr.write(`[plugin:${pluginId}:info] ${args.join(" ")}\n`),
        warn: (...args) => process.stderr.write(`[plugin:${pluginId}:warn] ${args.join(" ")}\n`),
        error: (...args) => process.stderr.write(`[plugin:${pluginId}:error] ${args.join(" ")}\n`)
      },
      process: {
        env: { ...process.env },
        cwd: process.cwd
      },
      Buffer,
      URL,
      setTimeout,
      clearTimeout,
      setInterval,
      clearInterval,
      definePlugin: (cfg) => cfg,
      __lastDefinedPlugin: null
    };

    const script = new vm.Script(jsCode, {
      filename: filePath,
      displayErrors: true
    });

    const context = vm.createContext(sandbox);
    script.runInContext(context, { timeout: 5000 });

    const pluginDef = sandbox.__lastDefinedPlugin;
    if (!pluginDef || typeof pluginDef !== "object") {
      return {
        success: false,
        error: new Error(`Plugin at ${filePath} did not export default definePlugin({ ... })`)
      };
    }

    const pluginName = pluginDef.name || pluginId;
    return {
      success: true,
      plugin: {
        id: pluginId,
        name: pluginName,
        version: pluginDef.version || "1.0.0",
        description: pluginDef.description || `Single-file plugin ${pluginName}`,
        filePath,
        mtime: fs.statSync(filePath).mtimeMs,
        systemPrompt: pluginDef.systemPrompt || null,
        tools: Array.isArray(pluginDef.tools) ? pluginDef.tools : [],
        onEvent: pluginDef.onEvent || null
      }
    };
  } catch (err) {
    return {
      success: false,
      error: err
    };
  }
}

// ---------------------------------------------------------------------------
// Execution Context Builder (Progressive Hybrid SDK)
// ---------------------------------------------------------------------------

function createPluginContext(pluginId, turnCtx = {}, callChain = []) {
  return {
    pluginId,
    workspaceDir: turnCtx.workspaceDir || process.cwd(),
    sessionId: turnCtx.sessionId || "default",
    turn: turnCtx.turn || 0,
    log: {
      info: (...args) => process.stderr.write(`[${pluginId}:info] ${args.join(" ")}\n`),
      warn: (...args) => process.stderr.write(`[${pluginId}:warn] ${args.join(" ")}\n`),
      error: (...args) => process.stderr.write(`[${pluginId}:error] ${args.join(" ")}\n`)
    },
    // Safe FS delegated to Rust's Onion Middleware (.arp/tmp atomic write + Path Jail)
    fs: {
      writeFile: async (relPath, content) => {
        return callRust("fs_write_file", { path: relPath, content });
      },
      readFile: async (relPath) => {
        return callRust("fs_read_file", { path: relPath });
      }
    },
    // Safe Shell command delegated to Rust's PGID tree-killing executor
    exec: async (command, options = {}) => {
      return callRust("exec_bash", { command, cwd: options.cwd });
    },
    // Inter-plugin tool calling with Cycle Guard & Max Depth 3
    callTool: async (toolName, toolArgs) => {
      if (callChain.includes(toolName)) {
        throw new Error(
          `[CycleGuard] Cyclic tool invocation detected! Chain: [${callChain.join(" -> ")} -> ${toolName}]`
        );
      }
      if (callChain.length >= MAX_CALL_DEPTH) {
        throw new Error(
          `[DepthGuard] Max inter-tool call depth (${MAX_CALL_DEPTH}) exceeded! Chain: [${callChain.join(" -> ")} -> ${toolName}]`
        );
      }

      const nextChain = [...callChain, toolName];

      // Find tool in active plugins
      for (const plugin of activePlugins.values()) {
        const found = plugin.tools.find((t) => t.name === toolName);
        if (found) {
          const subCtx = createPluginContext(plugin.id, turnCtx, nextChain);
          return await found.execute(toolArgs, subCtx);
        }
      }

      // If not a local TS tool, delegate to Rust to invoke built-in or MCP tools
      return callRust("call_tool", { tool: toolName, args: toolArgs });
    }
  };
}

// ---------------------------------------------------------------------------
// Blue-Green Atomic Swap & Manifest Sync
// ---------------------------------------------------------------------------

function broadcastManifest() {
  const pluginsSummary = [];
  const toolsSummary = [];

  for (const plugin of activePlugins.values()) {
    pluginsSummary.push({
      id: plugin.id,
      name: plugin.name,
      version: plugin.version,
      description: plugin.description,
      has_system_prompt: typeof plugin.systemPrompt === "function",
      has_on_event: typeof plugin.onEvent === "function"
    });

    for (const tool of plugin.tools) {
      toolsSummary.push({
        plugin_id: plugin.id,
        name: tool.name,
        description: tool.description || `Tool ${tool.name} from plugin ${plugin.name}`,
        parameters: tool.parameters || { type: "object", properties: {} }
      });
    }
  }

  sendToRust({
    type: "manifest_synced",
    plugins: pluginsSummary,
    tools: toolsSummary
  });
}

function applyPendingSwaps() {
  if (pendingSwapQueue.length === 0) return;
  if (inFlightExecutions > 0) {
    // Wait until in-flight executions finish before atomic swap
    return;
  }

  let changed = false;
  while (pendingSwapQueue.length > 0) {
    const item = pendingSwapQueue.shift();
    if (item.action === "update") {
      activePlugins.set(item.plugin.id, item.plugin);
      changed = true;
      process.stderr.write(`[ts-host] Blue-Green hot-reload applied for plugin '${item.plugin.id}'\n`);
    } else if (item.action === "remove") {
      activePlugins.delete(item.pluginId);
      changed = true;
      process.stderr.write(`[ts-host] Removed plugin '${item.pluginId}'\n`);
    }
  }

  if (changed) {
    broadcastManifest();
  }
}

// ---------------------------------------------------------------------------
// Discovery & Loading (Dual-Scope: Global + Workspace)
// ---------------------------------------------------------------------------

function scanAndLoad(pluginDirs) {
  for (const dir of pluginDirs) {
    if (!fs.existsSync(dir)) continue;
    const entries = fs.readdirSync(dir, { withFileTypes: true });
    for (const entry of entries) {
      if (entry.isFile() && (entry.name.endsWith(".ts") || entry.name.endsWith(".js"))) {
        const fullPath = path.join(dir, entry.name);
        const evalResult = shadowEvaluatePlugin(fullPath);
        if (evalResult.success) {
          activePlugins.set(evalResult.plugin.id, evalResult.plugin);
          process.stderr.write(`[ts-host] Loaded plugin: ${evalResult.plugin.id} (${fullPath})\n`);
        } else {
          process.stderr.write(
            `[ts-host:warning] Failed to load plugin at ${fullPath}: ${evalResult.error.message}\n`
          );
        }
      }
    }
  }
  broadcastManifest();
}

// ---------------------------------------------------------------------------
// STDIO Message Handler
// ---------------------------------------------------------------------------

const rl = readline.createInterface({
  input: process.stdin,
  output: process.stdout,
  terminal: false
});

rl.on("line", async (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;

  let msg;
  try {
    msg = JSON.parse(trimmed);
  } catch (err) {
    process.stderr.write(`[ts-host:malformed] ${trimmed}\n`);
    return;
  }

  // 1. Rust RPC response to an earlier callRust(...)
  if (msg.type === "rpc_response" && msg.id) {
    const pending = pendingRustRequests.get(msg.id);
    if (pending) {
      pendingRustRequests.delete(msg.id);
      clearTimeout(pending.timer);
      if (msg.success) {
        pending.resolve(msg.data);
      } else {
        pending.reject(new Error(msg.error || "Rust RPC error"));
      }
    }
    return;
  }

  // 2. Commands & RPC requests from Rust
  switch (msg.type) {
    case "init": {
      const pluginDirs = msg.plugin_dirs || [];
      scanAndLoad(pluginDirs);
      sendToRust({ type: "init_ack", success: true });
      break;
    }

    case "reload": {
      // Explicit or watched reload triggered from Rust
      const targetPath = msg.path;
      if (targetPath && fs.existsSync(targetPath)) {
        const evalResult = shadowEvaluatePlugin(targetPath);
        if (evalResult.success) {
          pendingSwapQueue.push({ action: "update", plugin: evalResult.plugin });
          applyPendingSwaps();
          sendToRust({ type: "reload_ack", success: true, plugin_id: evalResult.plugin.id });
        } else {
          // Syntax Error Immunity: log and refuse to swap
          process.stderr.write(
            `[ts-host:error-immune] Syntax/Init error in ${targetPath}. Keeping active version. Error: ${evalResult.error.message}\n`
          );
          sendToRust({
            type: "reload_ack",
            success: false,
            error: evalResult.error.message,
            kept_active: true
          });
        }
      } else {
        // Full rescanning of directories
        const pluginDirs = msg.plugin_dirs || [];
        scanAndLoad(pluginDirs);
        sendToRust({ type: "reload_ack", success: true });
      }
      break;
    }

    case "get_system_prompts": {
      const prompts = [];
      for (const plugin of activePlugins.values()) {
        if (typeof plugin.systemPrompt === "function") {
          try {
            const ctx = createPluginContext(plugin.id, msg.context || {});
            const text = await plugin.systemPrompt(ctx);
            if (typeof text === "string" && text.trim()) {
              prompts.push({
                plugin_id: plugin.id,
                plugin_name: plugin.name,
                prompt: text.trim()
              });
            }
          } catch (err) {
            process.stderr.write(`[ts-host] Error in systemPrompt for ${plugin.id}: ${err.message}\n`);
          }
        }
      }
      sendToRust({
        type: "system_prompts_result",
        request_id: msg.request_id,
        prompts
      });
      break;
    }

    case "dispatch_event": {
      // Capability A: Broadcast observed event to all active plugins
      const event = msg.event;
      const turnCtx = msg.context || {};
      for (const plugin of activePlugins.values()) {
        if (typeof plugin.onEvent === "function") {
          const ctx = createPluginContext(plugin.id, turnCtx);
          try {
            // Non-blocking fire & catch
            Promise.resolve(plugin.onEvent(event, ctx)).catch((err) => {
              process.stderr.write(`[ts-host] Error in onEvent for ${plugin.id}: ${err.message}\n`);
            });
          } catch (err) {
            process.stderr.write(`[ts-host] Synchronous error in onEvent for ${plugin.id}: ${err.message}\n`);
          }
        }
      }

      // Check for turn end to drain pending swaps
      if (event && (event.type === "turn_end" || event.type === "loop_complete")) {
        applyPendingSwaps();
      }
      break;
    }

    case "execute_tool": {
      // Capability C: Execute tool call
      const { call_id, tool_name, args, context: turnCtx } = msg;
      inFlightExecutions++;

      let foundTool = null;
      let targetPlugin = null;

      for (const plugin of activePlugins.values()) {
        const t = plugin.tools.find((item) => item.name === tool_name);
        if (t) {
          foundTool = t;
          targetPlugin = plugin;
          break;
        }
      }

      if (!foundTool) {
        inFlightExecutions--;
        sendToRust({
          type: "tool_result",
          call_id,
          success: false,
          error: `Tool '${tool_name}' not found in any active TypeScript plugin`
        });
        applyPendingSwaps();
        return;
      }

      try {
        const ctx = createPluginContext(targetPlugin.id, turnCtx, [tool_name]);
        const result = await foundTool.execute(args, ctx);
        const output = typeof result === "string" ? result : JSON.stringify(result, null, 2);
        sendToRust({
          type: "tool_result",
          call_id,
          success: true,
          output
        });
      } catch (err) {
        sendToRust({
          type: "tool_result",
          call_id,
          success: false,
          error: err instanceof Error ? err.stack || err.message : String(err)
        });
      } finally {
        inFlightExecutions--;
        applyPendingSwaps();
      }
      break;
    }

    default:
      process.stderr.write(`[ts-host:unknown_message_type] ${msg.type}\n`);
  }
});

process.stderr.write("[ts-host] Thunder TypeScript Plugin Sidecar initialized.\n");
