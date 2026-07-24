import { spawn, ChildProcess } from 'child_process';
import * as vscode from 'vscode';
import { AriadneConfig, AriadneMcpMessage, AriadneOperation } from './types';

export class AriadneClient {
  private config: AriadneConfig;
  private proc: ChildProcess | null = null;
  private nextId = 1;
  private pending = new Map<number | string, { resolve: (v: string) => void; reject: (e: Error) => void }>();
  private started = false;
  private initPromise: Promise<void> | null = null;

  constructor(config: AriadneConfig) {
    this.config = config;
  }

  private getBinaryPath(): string {
    return this.config.binaryPath || 'ariadne';
  }

  private getDbPath(): string {
    return this.config.dbPath || '';
  }

  private ensureStarted(): Promise<void> {
    if (this.initPromise) return this.initPromise;
    if (this.started) {
      this.initPromise = Promise.resolve();
      return this.initPromise;
    }
    this.initPromise = this.startServer();
    return this.initPromise;
  }

  private startServer(): Promise<void> {
    return new Promise((resolve, reject) => {
      const binary = this.getBinaryPath();
      const db = this.getDbPath();
      const args = ['--db', db, 'mcp-server'];

      this.proc = spawn(binary, args, {
        stdio: ['pipe', 'pipe', 'pipe'],
      });

      this.proc.stdout?.on('data', (data: Buffer) => this.handleStdout(data));
      this.proc.stderr?.on('data', (data: Buffer) => {
        const msg = data.toString().trim();
        if (msg) vscode.window.showWarningMessage(`Ariadne: ${msg}`);
      });

      this.proc.on('error', (err) => {
        reject(err);
        this.started = false;
        this.proc = null;
      });

      // Wait for initialized notification or a reasonable time
      const timeout = setTimeout(() => {
        this.started = true;
        resolve();
      }, 10000);

      this.proc.on('spawn', () => {
        clearTimeout(timeout);
        // Send MCP initialize request
        this.sendInit();
        this.started = true;
        resolve();
      });
    });
  }

  private sendInit() {
    if (!this.proc?.stdin) return;
    // Initialize notification
    this.proc.stdin.write(JSON.stringify({
      jsonrpc: '2.0',
      method: 'initialize',
      params: {
        protocolVersion: '2024-11-05',
        capabilities: {},
      },
    }) + '\n');
    // Sent notification
    this.proc.stdin.write(JSON.stringify({
      jsonrpc: '2.0',
      method: 'notifications/initialized',
    }) + '\n');
  }

  private handleStdout(data: Buffer) {
    const text = data.toString();
    const lines = text.split('\n').filter((l) => l.trim());

    for (const line of lines) {
      try {
        const msg: AriadneMcpMessage = JSON.parse(line);
        if (msg.id !== undefined) {
          const handler = this.pending.get(msg.id);
          if (handler) {
            this.pending.delete(msg.id);
            if (msg.error) {
              handler.reject(new Error(msg.error.message));
            } else if (msg.result) {
              const resp = msg.result.content?.[0]?.text || JSON.stringify(msg.result);
              handler.resolve(resp);
            }
          }
        }
        // Notifications without id (like initialized) are ignored
      } catch {
        // Not JSON, ignore
      }
    }
  }

  async callOperation(operation: AriadneOperation, params: Record<string, string> = {}): Promise<string> {
    await this.ensureStarted();

    if (!this.proc || !this.proc.stdin) {
      throw new Error('Ariadne server not running');
    }

    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });

      const request = JSON.stringify({
        jsonrpc: '2.0',
        id,
        method: 'tools/call',
        params: {
          name: 'graph',
          arguments: {
            operation,
            params: Object.entries(params).filter(([, v]) => v).reduce((acc, [k, v]) => {
              acc[k] = v;
              return acc;
            }, {} as Record<string, string>),
          },
        },
      });

      const writeOk = this.proc!.stdin!.write(request + '\n');
      if (!writeOk) {
        // Buffer full, wait for drain
        this.proc!.stdin!.once('drain', () => {
          // already written, just need to wait for response
        });
      }

      // Timeout
      setTimeout(() => {
        const handler = this.pending.get(id);
        if (handler) {
          this.pending.delete(id);
          handler.reject(new Error(`Ariadne operation "${operation}" timed out (30s)`));
        }
      }, 30000);
    });
  }

  async checkAvailable(): Promise<boolean> {
    try {
      const { spawn } = await import('child_process');
      await new Promise<void>((resolve, reject) => {
        const p = spawn(this.getBinaryPath(), ['--help'], { timeout: 5000 });
        p.stdout?.resume();
        p.on('close', (code) => code === 0 ? resolve() : reject(new Error(`exit ${code}`)));
        p.on('error', reject);
        setTimeout(() => { p.kill(); reject(new Error('timeout')); }, 5000);
      });
      return true;
    } catch {
      return false;
    }
  }

  dispose() {
    if (this.proc) {
      this.proc.kill();
      this.proc = null;
    }
  }
}
