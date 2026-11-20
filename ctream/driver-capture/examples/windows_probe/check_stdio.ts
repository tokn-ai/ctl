// Run with Bun, followed by the command that launches windows-probe --echo-stdio.
import { spawn } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";

const command = process.argv.slice(2);
if (command.length === 0) {
  throw new Error("Usage: bun check_stdio.ts <command> [arguments...]");
}

const payload = Buffer.alloc(8 * 1024 * 1024);
let state = 0x12345678;
for (let i = 0; i < payload.length; i++) {
  state ^= state << 13;
  state ^= state >>> 17;
  state ^= state << 5;
  payload[i] = state & 0xff;
}

type ProbeEvent = { event: string; pid: number; bytes?: number };

async function exercise(disconnect: boolean) {
  const child = spawn(command[0], command.slice(1), { stdio: "pipe" });
  let fault: Error | undefined;
  let received = 0;
  let stderr = "";
  let closed = false;
  const events: ProbeEvent[] = [];
  const completion = new Promise<{ code: number | null; signal: string | null }>((resolve) => {
    child.on("error", (error) => { fault = error; });
    child.on("close", (code, signal) => {
      closed = true;
      resolve({ code, signal });
    });
  });
  child.stdin.on("error", (error) => { fault = error; });
  child.stdout.on("data", (bytes: Buffer) => {
    if (!bytes.equals(payload.subarray(received, received + bytes.length))) {
      fault = new Error(`Binary mismatch at byte ${received}`);
    }
    received += bytes.length;
  });
  child.stderr.on("data", (bytes: Buffer) => {
    stderr += bytes.toString("utf8");
    let newline: number;
    while ((newline = stderr.indexOf("\n")) !== -1) {
      const line = stderr.slice(0, newline);
      stderr = stderr.slice(newline + 1);
      try {
        const event = JSON.parse(line);
        if (typeof event.event === "string" && Number.isInteger(event.pid)) events.push(event);
      } catch { /* SSH diagnostics are not probe events. */ }
    }
  });
  const deadline = Date.now() + 20_000;
  const watchdog = setTimeout(() => {
    fault = new Error("Transport check timed out");
    child.kill("SIGKILL");
  }, 20_000);
  async function until(ready: () => boolean) {
    while (!ready()) {
      if (fault) throw fault;
      if (closed) throw new Error("Command exited before the expected response");
      if (Date.now() >= deadline) throw new Error("Transport check timed out");
      await delay(5);
    }
    if (fault) throw fault;
  }
  try {
    await until(() => events.some((event) => event.event === "ready"));
    const ready = events.find((event) => event.event === "ready")!;
    const sizes = [1, 255, 65_537, 1_048_576];
    let sent = 0;
    let iteration = 0;
    const started = performance.now();
    const length = disconnect ? 65_793 : payload.length;
    while (sent < length) {
      const end = Math.min(length, sent + sizes[iteration++ % sizes.length]);
      await new Promise<void>((resolve, reject) => {
        child.stdin.write(payload.subarray(sent, end), (error) => error ? reject(error) : resolve());
      });
      sent = end;
      await until(() => received === sent);
    }
    const roundtrip_ms = Math.round(performance.now() - started);
    if (disconnect) {
      if (closed || !child.kill("SIGTERM")) throw new Error("Command exited before disconnect");
    } else child.stdin.end();
    const exit = await completion;
    if (fault) throw fault;
    if (!disconnect && (exit.code !== 0 || !events.some((event) => event.event === "eof" && event.bytes === sent))) {
      throw new Error("Expected clean EOF and matching remote byte count");
    }
    return {
      mode: disconnect ? "disconnect" : "eof",
      remote_pid: ready.pid,
      bytes_sent: sent,
      bytes_received: received,
      received_before_eof: true,
      roundtrip_ms,
      exit,
    };
  } finally {
    clearTimeout(watchdog);
    if (!closed) {
      child.kill("SIGKILL");
      await completion;
    }
  }
}

// Remote PID absence must be checked separately after the disconnect case.
console.log(JSON.stringify(await exercise(false)));
console.log(JSON.stringify(await exercise(true)));
