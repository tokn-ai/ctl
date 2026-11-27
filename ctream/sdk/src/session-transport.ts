import type { ChildProcessWithoutNullStreams } from "node:child_process";

const REQUEST_LIMIT = 64 * 1024;
const JSON_LIMIT = 2 * 1024 * 1024;
// ctream bounds frames at 33,554,432 RGB pixels. Allow PNG overhead and base64.
const SCREENSHOT_LIMIT = 144 * 1024 * 1024;
const STDERR_LIMIT = 8 * 1024;
// Rust allows up to 5s for input cleanup followed by 2s for capture cleanup.
const SHUTDOWN_MS = 10_000;
const ABORT_GRACE_MS = 1_000;

export type Request = {operation: string} & Record<string, unknown>;

type Success = {result: unknown; action_id?: string};

export class CtreamActionError extends Error {
  readonly action_id?: string;

  constructor(message: string, action_id?: string) {
    super(message);
    this.name = "CtreamActionError";
    this.action_id = action_id;
  }
}

/** A recoverable host control-turn error. owner_id identifies the current holder when available. */
export class CtreamTurnError extends CtreamActionError {
  readonly code: "busy" | "stale_turn";
  readonly owner_id?: string;

  constructor(message: string, code: "busy" | "stale_turn", owner_id?: string, action_id?: string) {
    super(message, action_id);
    this.name = "CtreamTurnError";
    this.code = code;
    this.owner_id = owner_id;
  }
}

type Child = ChildProcessWithoutNullStreams;
type Reply = {
  resolve: (value: Success) => void;
  reject: (error: Error) => void;
  limit: number;
  size: number;
  chunks: Buffer[];
};

export function reference(child: Child, method: "ref" | "unref"): void {
  // Bun's stdin lacks these methods. Its read pipes and Node's three pipes own handles.
  for (const handle of [child, child.stdin, child.stdout, child.stderr]) {
    (handle as {ref?: () => void; unref?: () => void})[method]?.();
  }
}

export class SessionTransport {
  readonly #launch: () => Child;
  readonly #timeout: number;
  readonly #shutdownMs: number;
  #child?: Child;
  #referenced = true;
  #exit?: Promise<void>;
  #tail = Promise.resolve();
  #closing?: Promise<void>;
  #ending = false;
  #failure?: Error;
  #pending?: Reply;
  #stderr: Buffer = Buffer.alloc(0);
  #eofTimer?: NodeJS.Timeout;
  #killTimer?: NodeJS.Timeout;

  constructor(launch: () => Child, timeout: number, shutdownMs = SHUTDOWN_MS) {
    this.#launch = launch;
    this.#timeout = timeout;
    this.#shutdownMs = shutdownMs;
  }

  /** Build, dispatch and validate one request before admitting the next. */
  request<T>(request: Request | (() => Request), decode: (value: unknown, action_id?: string) => T): Promise<T> {
    if (this.#closing) return Promise.reject(new Error("ctream session is closed"));
    const run = this.#tail.then(async () => {
      if (this.#failure) throw this.#failure;
      const body = typeof request === "function" ? request() : request;
      const payload = JSON.stringify(body);
      if (Buffer.byteLength(payload) > REQUEST_LIMIT) throw new RangeError("ctream request exceeds 64 KiB");
      const reply = await this.#exchange(payload, body.operation === "screenshot" ? SCREENSHOT_LIMIT : JSON_LIMIT);
      try {
        return decode(reply.result, reply.action_id);
      } catch (cause) {
        throw this.#fail(cause instanceof Error ? cause : new Error(String(cause)));
      }
    });
    this.#tail = run.then(() => {}, () => {});
    return run;
  }

  close(): Promise<void> {
    this.#closing ??= this.#finish();
    return this.#closing;
  }

  get closing(): boolean {
    return this.#closing !== undefined;
  }

  async #finish(): Promise<void> {
    // Each admitted request keeps its configured deadline. The shutdown deadline
    // bounds the subsequent wait for cleanup and exit after the last reply.
    await this.#tail;
    this.#ending = true;
    const child = this.#child;
    if (!child) {
      if (this.#failure) throw this.#failure;
      return;
    }
    this.#setReferenced(true);
    const deadline = setTimeout(() => this.#fail(new Error("ctream session close timed out")), this.#shutdownMs);
    try {
      child.stdin.end();
      await this.#exit;
      if (this.#failure) throw this.#failure;
    } finally {
      clearTimeout(deadline);
    }
  }

  #start(): Child {
    if (this.#child) return this.#child;
    const child = this.#launch();
    this.#child = child;
    this.#exit = new Promise((resolve) => {
      child.once("close", (code, signal) => {
        this.#child = undefined;
        clearTimeout(this.#eofTimer);
        clearTimeout(this.#killTimer);
        if (!this.#ending || code !== 0) {
          const diagnostic = this.#stderr.toString("utf8").trim();
          this.#fail(new Error(diagnostic || `ctream session exited (${signal ?? code})`));
        }
        resolve();
      });
    });
    child.on("error", (error) => this.#fail(error));
    child.stdout.on("data", (chunk: Buffer) => this.#receive(chunk));
    child.stdout.on("end", () => this.#awaitExit(new Error("ctream session output closed")));
    child.stdout.on("error", (error) => this.#fail(error));
    child.stderr.on("data", (chunk: Buffer) => {
      this.#stderr = Buffer.concat([this.#stderr, chunk.subarray(-STDERR_LIMIT)]).subarray(-STDERR_LIMIT);
    });
    child.stderr.on("error", (error) => this.#fail(error));
    child.stdin.on("error", (error) => this.#awaitExit(error));
    return child;
  }

  #awaitExit(error: Error): void {
    if (this.#ending || this.#failure || this.#eofTimer) return;
    // A terminating CLI can close stdout or reject a write before its exit status
    // and stderr arrive. A peer that only closed one pipe cannot serve another call.
    this.#eofTimer = setTimeout(() => {
      this.#fail(new Error(this.#stderr.toString("utf8").trim() || error.message, { cause: error }));
    }, 100).unref();
  }

  #setReferenced(active: boolean): void {
    if (this.#child && this.#referenced !== active) {
      reference(this.#child, active ? "ref" : "unref");
      this.#referenced = active;
    }
  }

  async #exchange(payload: string, limit: number): Promise<Success> {
    let child: Child;
    try {
      child = this.#start();
      this.#setReferenced(true);
      if (this.#eofTimer) {
        await this.#exit;
        throw this.#failure ?? new Error("ctream session output closed");
      }
    } catch (cause) {
      throw this.#fail(cause instanceof Error ? cause : new Error(String(cause)));
    }
    const timer = this.#timeout ? setTimeout(() => {
      this.#fail(new Error("ctream session request timed out; its outcome is unknown"));
    }, this.#timeout) : undefined;
    try {
      return await new Promise((resolve, reject) => {
        this.#pending = {resolve, reject, limit, size: 0, chunks: []};
        try {
          child.stdin.write(payload + "\n");
        } catch (cause) {
          this.#fail(cause instanceof Error ? cause : new Error(String(cause)));
        }
      });
    } finally {
      clearTimeout(timer);
      if (!this.#closing && !this.#failure) this.#setReferenced(false);
    }
  }

  #fail(error: Error): Error {
    if (this.#failure) return this.#failure;
    this.#failure = error;
    this.#pending?.reject(error);
    this.#pending = undefined;
    const child = this.#child;
    if (child) {
      // Deliver EOF so the CLI can release remote channels. Kill only if cleanup stalls.
      child.stdin.end();
      this.#killTimer = setTimeout(() => {
        child.kill("SIGKILL");
        child.stdin.destroy();
        child.stdout.destroy();
        child.stderr.destroy();
      }, ABORT_GRACE_MS).unref();
    }
    return error;
  }

  #receive(chunk: Buffer): void {
    if (this.#failure) return;
    const pending = this.#pending;
    if (!pending) {
      this.#fail(new Error("unexpected ctream session output"));
      return;
    }
    const newline = chunk.indexOf(10);
    const bytes = newline < 0 ? chunk : chunk.subarray(0, newline);
    pending.size += bytes.length;
    if (pending.size > pending.limit) {
      this.#fail(new Error("ctream session reply exceeds its byte limit"));
      return;
    }
    pending.chunks.push(bytes);
    if (newline < 0) return;
    if (newline !== chunk.length - 1) {
      this.#fail(new Error("unexpected data after the ctream session reply"));
      return;
    }
    try {
      const bytes = Buffer.concat(pending.chunks, pending.size);
      const envelope = JSON.parse(new TextDecoder("utf-8", {fatal: true}).decode(bytes));
      const action_id: unknown = envelope?.action_id;
      if (action_id !== undefined && (typeof action_id !== "string" ||
          !/^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/.test(action_id))) {
        throw new Error("invalid action ID");
      }
      if (envelope?.ok === true && Object.hasOwn(envelope, "result")) {
        pending.resolve({result: envelope.result, action_id});
      } else if (envelope?.ok === false && typeof envelope.error === "string") {
        if (envelope.code === "busy" || envelope.code === "stale_turn") {
          if (envelope.owner_id !== undefined && typeof envelope.owner_id !== "string") {
            throw new Error("invalid turn owner ID");
          }
          pending.reject(new CtreamTurnError(envelope.error, envelope.code, envelope.owner_id, action_id));
        } else {
          pending.reject(new CtreamActionError(envelope.error, action_id));
        }
      } else {
        throw new Error("unexpected envelope");
      }
      this.#pending = undefined;
    } catch (cause) {
      this.#fail(new Error("invalid ctream session reply", {cause}));
    }
  }
}
