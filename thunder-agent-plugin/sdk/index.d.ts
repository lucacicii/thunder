/**
 * Type definitions for @thunder-agent/sdk
 * Single-file plugin development for Thunder Agent.
 */

export interface ToolContext {
  pluginId: string;
  workspaceDir: string;
  sessionId: string;
  turn: number;
  log: {
    info(...args: any[]): void;
    warn(...args: any[]): void;
    error(...args: any[]): void;
  };
  /**
   * Safe file system operations delegated to Rust's Onion Middleware:
   * Uses `.arp/tmp/` atomic shadow staging, hardware fsync, and Path Jail verification.
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
   * Inter-tool call bus with automatic cycle guard and maximum depth limit of 3.
   */
  callTool(toolName: string, args: Record<string, any>): Promise<any>;
}

export interface ToolDefinition {
  name: string;
  description: string;
  parameters?: Record<string, any>;
  execute(args: any, ctx: ToolContext): Promise<string | any> | string | any;
}

export interface ObservedEvent {
  type: string;
  data?: any;
  timestamp?: number;
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
