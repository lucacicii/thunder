/**
 * Type definitions for @thunder-agent/sdk
 * Single-file plugin development for Thunder Agent.
 */

export interface ToolContext {
  pluginId: string;
  workspaceDir: string;
  /**
   * The session this call belongs to. `undefined` for a tool call that carried
   * no run identifier — in which case every privileged call below is refused.
   */
  sessionId: string | undefined;
  turn: number;
  log: {
    info(...args: any[]): void;
    warn(...args: any[]): void;
    error(...args: any[]): void;
  };
  /**
   * Safe file system operations delegated to Rust's Onion Middleware:
   * Uses `.thunder/tmp/` atomic shadow staging, hardware fsync, and Path Jail verification.
   */
  fs: {
    writeFile(relPath: string, content: string): Promise<{ success: boolean; path: string; bytesWritten: number }>;
    readFile(relPath: string): Promise<string>;
  };
  /**
   * Safe shell execution delegated to Rust's PGID tree-killing executor.
   */
  exec(command: string, options?: { cwd?: string }): Promise<{ exitCode: number; stdout: string; stderr: string }>;
  /**
   * Call another tool.
   *
   * Resolves locally when the target belongs to a TypeScript plugin in this
   * sidecar (with a cycle guard and a maximum depth of 3), and is handed to the
   * host for anything else — built-in tools, MCP tools, skill tools.
   *
   * A host-dispatched call runs through the **same** pipeline a model-initiated
   * call does, so it cannot be used to escape the run's permission tier, the
   * path jail, or the approval dialog. When approval is required the dialog
   * names this plugin, so the user is approving the plugin's request and not
   * mistaking it for the assistant's. Throws with the layer's own message when
   * the call is refused.
   */
  callTool(toolName: string, args: Record<string, any>): Promise<any>;
  /**
   * User interaction, mediated by the host. Nothing is rendered here: a request
   * travels to the host as plain data and the host decides whether to show it.
   *
   * Every request is tagged `source: "plugin"` on the wire. A host panel must
   * render plugin prompts distinctly from its own permission prompts and must
   * never treat an answer here as an authorisation decision — permissions are
   * decided by the host, not by a dialog a plugin asked for.
   *
   * All of these fail closed: with no panel attached, dismissed, or after the
   * 60s deadline, `select` / `input` / `editor` resolve to `null` and `confirm`
   * to `false`. Treat that as "no".
   */
  ui: {
    /** Pick one of `options`; `null` when unanswered. */
    select(title: string, options: string[]): Promise<string | null>;
    /** Yes/no. `false` when unanswered — never treat as consent. */
    confirm(title: string, message: string): Promise<boolean>;
    /** Single-line free text; `null` when unanswered. */
    input(title: string, placeholder?: string): Promise<string | null>;
    /** Multi-line free text; `null` when unanswered. */
    editor(title: string, prefill?: string): Promise<string | null>;
    /** Fire-and-forget notification. Silently dropped if nothing is listening. */
    notify(message: string, level?: "info" | "warning" | "error"): void;
    /** Set or clear (`undefined`) a status entry in the host's status bar. */
    setStatus(key: string, text: string | undefined): void;
  };
}

export interface ToolDefinition {
  name: string;
  description: string;
  parameters?: Record<string, any>;
  execute(args: any, ctx: ToolContext): Promise<string | any> | string | any;
}

/**
 * A lifecycle event from the agent loop.
 *
 * The wire shape is the loop's `ObservedEvent`, which nests the payload one
 * level down: read `event.event.type`, not `event.type`. Field names are
 * `snake_case`.
 */
export interface ObservedEvent {
  agent_id: string;
  event: {
    type: string;
    [key: string]: any;
  };
}

export interface PluginDefinition {
  name: string;
  version?: string;
  description?: string;
  /**
   * Capability B: Dynamically inject system prompt into LLM context.
   */
  systemPrompt?: (ctx: ToolContext) => Promise<string> | string;
  /**
   * Capability C: Expose dynamic tools to the LLM.
   */
  tools?: ToolDefinition[];
  /**
   * Capability A: Observe lifecycle events from the agent loop.
   */
  onEvent?: (event: ObservedEvent, ctx: ToolContext) => Promise<void> | void;
}

/**
 * Define a Thunder Agent TypeScript Plugin.
 */
export function definePlugin(config: PluginDefinition): PluginDefinition;
