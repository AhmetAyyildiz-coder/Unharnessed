import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { createInterface } from "node:readline";
import { Readable, Writable } from "node:stream";
import * as acp from "@agentclientprotocol/sdk";
import type * as vscode from "vscode";
import type { AgentState } from "./protocol";

const STDERR_TAIL = 20;
const INITIALIZE_TIMEOUT_MS = 30_000;
/** How long `stop` waits after closing stdin, then after SIGTERM. */
const EXIT_GRACE_MS = 2_000;

/**
 * One `unharnessed --mode acp` process and its ACP connection. The state
 * moves starting → ready → failed (crash), and back to starting on restart.
 */
export class Agent {
  private state: AgentState;
  private proc: ChildProcessWithoutNullStreams | undefined;
  private connection: acp.ClientConnection | undefined;
  private stderr: string[] = [];
  private restarting: Promise<void> | undefined;
  private readonly listeners = new Set<(state: AgentState) => void>();

  constructor(
    private readonly binary: string,
    private readonly cwd: string,
    private readonly version: string,
    private readonly log: vscode.LogOutputChannel,
  ) {
    this.state = { kind: "starting" };
  }

  get current(): AgentState {
    return this.state;
  }

  onChange(listener: (state: AgentState) => void): { dispose(): void } {
    this.listeners.add(listener);
    return { dispose: () => this.listeners.delete(listener) };
  }

  start(): void {
    this.stderr = [];
    this.set({ kind: "starting" });
    this.log.info(`starting agent: ${this.binary} --mode acp (cwd ${this.cwd})`);

    const proc = spawn(this.binary, ["--mode", "acp"], { cwd: this.cwd, stdio: "pipe" });
    this.proc = proc;
    // ENOENT / EACCES arrive here, without an exit event.
    proc.on("error", (error) => this.fail(proc, error.message));
    proc.on("exit", (code, signal) => this.fail(proc, `agent exited (${signal ?? `code ${code}`})`));
    // Writing to a process that already died raises EPIPE on stdin.
    proc.stdin.on("error", (error) => this.log.warn(`agent stdin: ${error.message}`));
    createInterface({ input: proc.stderr }).on("line", (line) => this.onStderr(proc, line));

    const stream = acp.ndJsonStream(
      Writable.toWeb(proc.stdin) as WritableStream<Uint8Array>,
      Readable.toWeb(proc.stdout) as ReadableStream<Uint8Array>,
    );
    const connection = acp.client({ name: "unharnessed-vscode" }).connect(logged(stream, this.log));
    this.connection = connection;

    // Nothing is written before the process exists: a missing binary never
    // sees a write (which would raise SIGPIPE in the extension host).
    proc.once("spawn", () => {
      this.log.info(`agent started (pid ${proc.pid})`);
      this.initialize(proc, connection);
    });
  }

  /** A second call while one is stopping the agent joins it. */
  restart(): Promise<void> {
    this.restarting ??= this.stop()
      .then(() => this.start())
      .finally(() => (this.restarting = undefined));
    return this.restarting;
  }

  /** Closes stdin (the agent exits on EOF), then SIGTERM, then SIGKILL. */
  async stop(): Promise<void> {
    const proc = this.proc;
    this.proc = undefined;
    this.connection?.close();
    this.connection = undefined;
    if (!proc || proc.pid === undefined || hasExited(proc)) return;

    proc.stdin.end();
    if (await exitsWithin(proc, EXIT_GRACE_MS)) return;
    this.log.warn(`agent ${proc.pid} still running ${EXIT_GRACE_MS} ms after stdin closed: SIGTERM`);
    proc.kill("SIGTERM");
    if (await exitsWithin(proc, EXIT_GRACE_MS)) return;
    this.log.warn(`agent ${proc.pid} ignored SIGTERM: SIGKILL`);
    proc.kill("SIGKILL");
  }

  private initialize(proc: ChildProcessWithoutNullStreams, connection: acp.ClientConnection): void {
    const initialize = connection.agent.request("initialize", {
      protocolVersion: acp.PROTOCOL_VERSION,
      clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false },
      clientInfo: { name: "unharnessed-vscode", title: "Unharnessed for VS Code", version: this.version },
    });
    withTimeout(initialize, INITIALIZE_TIMEOUT_MS, "initialize").then(
      (result) => {
        if (proc !== this.proc || this.state.kind !== "starting") return;
        const { name = "agent", version = "unknown" } = result.agentInfo ?? {};
        this.log.info(`agent ready: ${name} ${version} (protocol ${result.protocolVersion}, pid ${proc.pid})`);
        this.set({ kind: "ready" });
      },
      async (error: unknown) => {
        // A process that dies closes the connection before its exit event:
        // let the exit handler report the exit code instead.
        if (await exitsWithin(proc, EXIT_GRACE_MS)) return;
        this.fail(proc, `initialize failed: ${errorText(error)}`);
        void this.stop();
      },
    );
  }

  private onStderr(proc: ChildProcessWithoutNullStreams, line: string): void {
    this.log.info(`[agent] ${line}`);
    if (proc !== this.proc) return;
    this.stderr = [...this.stderr, line].slice(-STDERR_TAIL);
  }

  /** Ignores events from a process that was already replaced or stopped. */
  private fail(proc: ChildProcessWithoutNullStreams, error: string): void {
    if (proc !== this.proc || this.state.kind === "failed") return;
    this.log.error(`agent failed: ${error}`);
    this.connection?.close();
    this.set({ kind: "failed", error, stderr: this.stderr });
  }

  private set(state: AgentState): void {
    this.state = state;
    for (const listener of this.listeners) listener(state);
  }
}

/** Writes every JSON-RPC frame to the log: `→` sent, `←` received. */
function logged(stream: acp.Stream, log: vscode.LogOutputChannel): acp.Stream {
  const tap = (arrow: string) =>
    new TransformStream<acp.AnyMessage, acp.AnyMessage>({
      transform(message, controller) {
        log.info(`${arrow} ${JSON.stringify(message)}`);
        controller.enqueue(message);
      },
    });
  const outgoing = tap("→");
  outgoing.readable.pipeTo(stream.writable).catch(() => {});
  return { readable: stream.readable.pipeThrough(tap("←")), writable: outgoing.writable };
}

function withTimeout<T>(promise: Promise<T>, ms: number, what: string): Promise<T> {
  let timer: NodeJS.Timeout | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(`no ${what} answer after ${ms / 1000} s`)), ms);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

function hasExited(proc: ChildProcessWithoutNullStreams): boolean {
  return proc.exitCode !== null || proc.signalCode !== null;
}

/** Whether `proc` has exited, or exits within `ms`. */
function exitsWithin(proc: ChildProcessWithoutNullStreams, ms: number): Promise<boolean> {
  if (hasExited(proc)) return Promise.resolve(true);
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      proc.off("exit", onExit);
      resolve(false);
    }, ms);
    const onExit = () => {
      clearTimeout(timer);
      resolve(true);
    };
    proc.once("exit", onExit);
  });
}

function errorText(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "object" && error && "message" in error) return String(error.message);
  return String(error);
}
