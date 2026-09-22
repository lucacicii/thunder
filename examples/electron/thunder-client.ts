/**
 * Thunder Agent Daemon — Electron / Node.js Sidecar Client
 *
 * This client manages the lifecycle of the Rust `thunder-daemon` subprocess,
 * communicates over STDIO using line-delimited JSON (NDJSON), handles request/response
 * matching, and emits streaming token/tool events.
 */

import { ChildProcess, spawn } from 'child_process';
import { EventEmitter } from 'events';
import * as path from 'path';
import * as readline from 'readline';

export interface ThunderClientOptions {
  /** Path to the thunder-daemon binary. If omitted, auto-detects dev/prod path. */
  binaryPath?: string;
  /** Working directory for agent workspace. */
  workspaceDir?: string;
  /** Force mock mode (useful for UI development without API keys). */
  useMock?: boolean;
}

export interface ModelInfo {
  id: string;
  provider: string;
  name: string;
  selection_id: string;
  available: boolean;
}

export interface RunTaskOptions {
  taskId: string;
  prompt: string;
  sessionId?: string;
  model?: string;
  useMock?: boolean;
  workspaceDir?: string;
  /** Callback for real-time observed events (tokens, tools, turns) */
  onEvent?: (event: any) => void;
}

export class ThunderClient extends EventEmitter {
  private process: ChildProcess | null = null;
  private lineReader: readline.Interface | null = null;
  private pendingRequests = new Map<
    string,
    { resolve: (data: any) => void; reject: (err: any) => void }
  >();
  private activeTaskListeners = new Map<string, (event: any) => void>();
  private reqSeq = 0;
  private options: ThunderClientOptions;

  constructor(options: ThunderClientOptions = {}) {
    super();
    this.options = options;
  }

  /**
   * Start the Rust sidecar daemon process.
   */
  public start(): void {
    if (this.process) {
      return;
    }

    const binaryPath = this.options.binaryPath || this.resolveBinaryPath();
    const args: string[] = [];
    if (this.options.workspaceDir) {
      args.push('--workspace', this.options.workspaceDir);
    }

    this.process = spawn(binaryPath, args, {
      stdio: ['pipe', 'pipe', 'inherit'], // stderr is passed through to parent terminal
      env: {
        ...process.env,
      },
    });

    this.lineReader = readline.createInterface({
      input: this.process.stdout!,
      crlfDelay: Infinity,
    });

    this.lineReader.on('line', (line) => {
      this.handleLine(line);
    });

    this.process.on('exit', (code, signal) => {
      this.emit('exit', { code, signal });
      this.process = null;
      this.lineReader = null;
      // Reject any pending requests
      for (const [_, pending] of this.pendingRequests) {
        pending.reject(new Error(`Daemon exited with code ${code}`));
      }
      this.pendingRequests.clear();
      this.activeTaskListeners.clear();
    });

    this.process.on('error', (err) => {
      this.emit('error', err);
    });
  }

  /**
   * Stop the daemon process.
   */
  public stop(): void {
    if (this.process) {
      this.process.kill();
      this.process = null;
      this.lineReader = null;
    }
  }

  /**
   * Ping daemon to test connectivity.
   */
  public async ping(): Promise<{ pong: boolean; version: string }> {
    return this.sendRequest('ping', {});
  }

  /**
   * List all configured models.
   */
  public async listModels(): Promise<ModelInfo[]> {
    const res = await this.sendRequest('list_models', {});
    return res.models || [];
  }

  /**
   * List stored conversations.
   */
  public async listConversations(): Promise<any[]> {
    return this.sendRequest('list_conversations', {});
  }

  /**
   * Get details for a specific conversation.
   */
  public async getConversation(sessionId: string): Promise<any> {
    return this.sendRequest('get_conversation', { session_id: sessionId });
  }

  /**
   * Run an Agent task and stream tokens/tool events.
   */
  public async runTask(opts: RunTaskOptions): Promise<any> {
    if (opts.onEvent) {
      this.activeTaskListeners.set(opts.taskId, opts.onEvent);
    }

    return new Promise((resolve, reject) => {
      const taskDoneListener = (evt: any) => {
        if (evt.task_id === opts.taskId) {
          this.activeTaskListeners.delete(opts.taskId);
          this.off('task_completed', onCompleted);
          this.off('task_failed', onFailed);
          resolve(evt);
        }
      };

      const taskFailedListener = (evt: any) => {
        if (evt.task_id === opts.taskId) {
          this.activeTaskListeners.delete(opts.taskId);
          this.off('task_completed', onCompleted);
          this.off('task_failed', onFailed);
          reject(new Error(evt.error || 'Task failed'));
        }
      };

      const onCompleted = taskDoneListener;
      const onFailed = taskFailedListener;

      this.on('task_completed', onCompleted);
      this.on('task_failed', onFailed);

      this.sendRequest('run_task', {
        task_id: opts.taskId,
        prompt: opts.prompt,
        session_id: opts.sessionId,
        model: opts.model,
        use_mock: opts.useMock ?? this.options.useMock ?? false,
        workspace_dir: opts.workspaceDir ?? this.options.workspaceDir,
      }).catch((err) => {
        this.activeTaskListeners.delete(opts.taskId);
        this.off('task_completed', onCompleted);
        this.off('task_failed', onFailed);
        reject(err);
      });
    });
  }

  /**
   * Cancel an in-progress task.
   */
  public async cancelTask(taskId: string): Promise<boolean> {
    const res = await this.sendRequest('cancel_task', { task_id: taskId });
    return res?.cancelled ?? false;
  }

  private sendRequest(method: string, params: Record<string, any>): Promise<any> {
    return new Promise((resolve, reject) => {
      if (!this.process || !this.process.stdin?.writable) {
        return reject(new Error('Thunder daemon process is not running or stdin closed'));
      }

      const id = `req_${++this.reqSeq}_${Date.now()}`;
      this.pendingRequests.set(id, { resolve, reject });

      const payload = {
        method,
        id,
        ...params,
      };

      this.process.stdin.write(JSON.stringify(payload) + '\n', (err) => {
        if (err) {
          this.pendingRequests.delete(id);
          reject(err);
        }
      });
    });
  }

  private handleLine(line: string): void {
    const trimmed = line.trim();
    if (!trimmed) return;

    try {
      const msg = JSON.parse(trimmed);
      const type = msg.type;

      if (type === 'response') {
        const id = msg.id;
        if (id && this.pendingRequests.has(id)) {
          const { resolve, reject } = this.pendingRequests.get(id)!;
          this.pendingRequests.delete(id);
          if (msg.success) {
            resolve(msg.data);
          } else {
            reject(new Error(msg.error || 'Command failed'));
          }
        }
      } else if (type === 'observed_event') {
        const taskId = msg.task_id;
        const listener = this.activeTaskListeners.get(taskId);
        if (listener) {
          listener(msg.event);
        }
        this.emit('observed_event', msg);
      } else if (type === 'task_completed') {
        this.emit('task_completed', msg);
      } else if (type === 'task_failed') {
        this.emit('task_failed', msg);
      }
    } catch (e) {
      console.error('[ThunderClient] Failed to parse daemon output line:', trimmed, e);
    }
  }

  private resolveBinaryPath(): string {
    const isWin = process.platform === 'win32';
    const binName = isWin ? 'thunder-daemon.exe' : 'thunder-daemon';

    // In Electron packaged app, binary is located in process.resourcesPath/bin
    if (typeof process !== 'undefined' && (process as any).resourcesPath) {
      return path.join((process as any).resourcesPath, 'bin', binName);
    }

    // Default to workspace cargo build target (debug/release)
    return path.join(__dirname, '../../thunder-agent-daemon/target/debug', binName);
  }
}
