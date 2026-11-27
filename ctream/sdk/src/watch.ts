import type { ChildProcessWithoutNullStreams } from "node:child_process";
import { PassThrough, type Readable } from "node:stream";
import { liveEvent, type LiveEvent } from "./live-event.js";
import { reference } from "./session-transport.js";

const RECORD_LIMIT = 64 * 1024;
const STDERR_LIMIT = 8 * 1024;
const SHUTDOWN_MS = 5_000;
const BUN_BACKLOG_LIMIT = 1024 * 1024;

// Bun's subprocess pipes keep collecting stdout even while their Node Readable
// is paused. Keep draining the native pipe and disconnect a stalled subscriber
// at a known byte limit. Node's subprocess pipes backpressure the CLI directly.
function watchOutput(stdout: Readable, overflow: (error: Error) => void): Readable {
  if (!process.versions.bun) return stdout;
  const output = new PassThrough();
  output.on("error", () => {}); // An iterator can be suspended between reads.
  stdout.on("data", (chunk: Buffer) => {
    if (output.destroyed || output.writableEnded) return;
    if (output.readableLength + output.writableLength + chunk.length > BUN_BACKLOG_LIMIT) {
      const error = new Error("ctream watch consumer exceeded its 1 MiB backlog");
      output.destroy(error);
      overflow(error);
    } else {
      output.write(chunk);
    }
  });
  stdout.on("error", (error) => output.destroy(error));
  const end = () => { if (!output.writableEnded) output.end(); };
  stdout.once("end", end);
  stdout.once("close", end); // Bun can close without "end" after a spawn error.
  return output;
}

async function* records(stream: Readable, turns: boolean): AsyncGenerator<LiveEvent> {
  const decoder = new TextDecoder("utf-8", {fatal: true});
  let parts: Buffer[] = [];
  let size = 0;
  // Pull only as the consumer iterates. A slow consumer fills the pipe instead
  // of accumulating parsed events in memory. Cleanup may still drain the pipe.
  for await (const chunk of stream.iterator({destroyOnReturn: false}) as AsyncIterable<Buffer>) {
    let start = 0;
    while (start < chunk.length) {
      const lf = chunk.indexOf(10, start);
      const part = chunk.subarray(start, lf === -1 ? chunk.length : lf);
      size += part.length;
      if (size > RECORD_LIMIT) throw new Error("ctream watch record exceeds 64 KiB");
      parts.push(part);
      if (lf === -1) break;
      const bytes = parts.length === 1 ? part : Buffer.concat(parts, size);
      parts = [];
      size = 0;
      start = lf + 1;
      let value: unknown;
      try {
        value = JSON.parse(decoder.decode(bytes));
      } catch (cause) {
        throw new Error("invalid JSON in ctream watch record", {cause});
      }
      yield liveEvent(value, turns);
    }
  }
  if (size) throw new Error("truncated ctream watch record");
}

export class CtreamWatch implements AsyncIterableIterator<LiveEvent, undefined> {
  readonly #launch: () => ChildProcessWithoutNullStreams;
  readonly #turns: boolean;
  readonly #reader: AsyncGenerator<LiveEvent, undefined>;
  #child?: ChildProcessWithoutNullStreams;
  #exit?: Promise<Error | undefined>;
  #output?: PassThrough;
  #shutdown?: Promise<void>;
  #closing?: Promise<void>;
  #reading = false;
  #referenced = true;

  constructor(launch: () => ChildProcessWithoutNullStreams, turns: boolean) {
    this.#launch = launch;
    this.#turns = turns;
    this.#reader = this.#events();
  }

  [Symbol.asyncIterator](): this {
    return this;
  }

  async next(): Promise<IteratorResult<LiveEvent, undefined>> {
    if (this.#closing) return {done: true, value: undefined};
    if (this.#reading) throw new Error("ctream watch already has a pending read");
    this.#reading = true;
    this.#setReferenced(true);
    try {
      return await this.#reader.next();
    } finally {
      this.#reading = false;
      if (!this.#shutdown) this.#setReferenced(false);
    }
  }

  async return(): Promise<IteratorResult<LiveEvent, undefined>> {
    await this.close();
    return {done: true, value: undefined};
  }

  /** Leave the stream and wait for the local CLI to exit, interrupting a pending read. */
  close(): Promise<void> {
    this.#closing ??= this.#stop().finally(() => this.#reader.return(undefined));
    return this.#closing;
  }

  [Symbol.asyncDispose](): Promise<void> {
    return this.close();
  }

  #setReferenced(active: boolean): void {
    if (this.#child && this.#referenced !== active) {
      reference(this.#child, active ? "ref" : "unref");
      this.#referenced = active;
    }
  }

  async *#events(): AsyncGenerator<LiveEvent, undefined> {
    const child = this.#launch();
    this.#child = child;
    let stderr = Buffer.alloc(0);
    let failure: Error | undefined;
    child.once("error", (error) => { failure = error; });
    child.stderr.on("error", (error) => { failure = error; });
    child.stderr.on("data", (chunk: Buffer) => {
      stderr = Buffer.concat([stderr, chunk.subarray(-STDERR_LIMIT)]).subarray(-STDERR_LIMIT);
    });
    // Stdin carries no data. A peer that already closed it reports its status on exit.
    child.stdin.on("error", () => {});
    this.#exit = new Promise((resolve) => {
      child.once("close", (code, signal) => {
        this.#child = undefined;
        if (code !== 0) failure ??= new Error(stderr.toString("utf8").trim() || `ctream watch exited (${signal ?? code})`);
        resolve(failure);
      });
    });

    let first = true;
    let awaitingTurn = this.#turns;
    const output = watchOutput(child.stdout, (error) => {
      failure = error;
      this.#stop().catch(() => {});
    });
    if (output instanceof PassThrough) this.#output = output;
    try {
      for await (const event of records(output, this.#turns)) {
        if (this.#closing) break;
        if (first !== (event.type === "snapshot")) throw new Error("expected exactly one initial ctream watch snapshot");
        if (!first && awaitingTurn) {
          if (event.type !== "turn") throw new Error("expected initial control turn after ctream watch snapshot");
          awaitingTurn = false;
        }
        if (event.type === "turn" && !this.#turns) throw new Error("unexpected control turn on ctream watch");
        first = false;
        yield event;
      }
    } catch (error) {
      await this.#stop().catch(() => {});
      throw error;
    }
    await this.#stop();
    if (first && !this.#closing) throw new Error("ctream watch ended before its initial snapshot");
    if (awaitingTurn && !this.#closing) throw new Error("ctream watch ended before its initial control turn");
  }

  #stop(): Promise<void> {
    this.#shutdown ??= this.#finish();
    return this.#shutdown;
  }

  async #finish(): Promise<void> {
    const child = this.#child;
    let deadline: NodeJS.Timeout | undefined;
    if (child) {
      this.#setReferenced(true);
      child.stdin.end();
      // A paused async iterator owns a readable listener. resume() alone cannot
      // drain that stream when the CLI is blocked writing its remaining output.
      const discard = () => { while (child.stdout.read() !== null) {} };
      child.stdout.on("readable", discard);
      discard();
      this.#output?.end();
      deadline = setTimeout(() => {
        child.kill("SIGKILL");
        child.stdin.destroy();
        child.stdout.destroy();
        child.stderr.destroy();
      }, SHUTDOWN_MS);
    }
    try {
      const failure = await this.#exit;
      if (failure) throw failure;
    } finally {
      clearTimeout(deadline);
      this.#output = undefined;
    }
  }
}
