import { spawn } from "node:child_process";
import { connect } from "node:net";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { chmodSync, mkdtempSync, rmSync } from "node:fs";

const ctl = process.argv[2];
const inherited = process.argv[3] === "--fd";
const password = "  caller-credential-☃-canary  ";
const input = "command stdin remains independent\n";

async function run(payload: string | Uint8Array, close = true, plain = false) {
  const windows = process.platform === "win32";
  const directory = !windows && !inherited ? mkdtempSync("/tmp/ctl-pw-") : undefined;
  if (directory) chmodSync(directory, 0o700);
  const endpoint = windows ? `\\\\.\\pipe\\ctl-password-${randomUUID()}` : `${directory}/password.sock`;
  const options = inherited ? ["--password-fd", "3"]
    : [windows ? "--password-pipe" : "--password-socket", endpoint];
  const child = spawn(ctl, ["--destination", "caller@[::1]", ...options,
    ...(plain ? ["shell", "--plain"] : ["exec", "--", "cat"])], {
    stdio: inherited ? ["pipe", "pipe", "pipe", "pipe"] : ["pipe", "pipe", "pipe"],
  });
  let stdout = "", stderr = "";
  child.stdout!.on("data", (bytes) => { stdout += bytes.toString(); });
  child.stderr!.on("data", (bytes) => { stderr += bytes.toString(); });
  const exited = new Promise<number | null>((resolve, reject) => {
    child.once("error", reject);
    child.once("close", resolve);
  });
  const timer = setTimeout(() => child.kill(), 15000);
  let channel: import("node:stream").Writable;
  if (inherited) {
    channel = child.stdio[3] as import("node:stream").Writable;
  } else {
    const deadline = Date.now() + 4000;
    while (true) {
      try {
        channel = await new Promise<import("node:net").Socket>((resolve, reject) => {
          const socket = connect(endpoint);
          socket.once("error", reject);
          socket.once("connect", () => { socket.removeListener("error", reject); resolve(socket); });
        });
        break;
      } catch (error) {
        if (Date.now() >= deadline) { child.kill(); throw error; }
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
    }
  }
  channel.on("error", () => {});
  channel.write(Buffer.from(payload));
  if (close) channel.end();
  if (close) child.stdin!.end(input); else child.stdin!.write(input);
  const code = await exited;
  channel.destroy();
  clearTimeout(timer);
  if (directory) rmSync(directory, { recursive: true, force: true });
  assert(!stdout.includes(password) && !stderr.includes(password), "password was disclosed");
  return { code, stdout, stderr };
}

for (const plain of [false, true]) {
  const result = await run(password, true, plain);
  assert.equal(result.code, 17, result.stderr);
  assert.equal(result.stdout, input);
  assert(result.stderr.includes("separate-command-stderr"));
}
const wrong = await run("wrong supplied credential");
assert.equal(wrong.code, 255, wrong.stderr);
for (const payload of ["", "bad\npassword", "x".repeat(4097), "bad\u0000password", Buffer.from([0xff])]) {
  const result = await run(payload);
  assert.equal(result.code, 1, result.stderr);
  assert(!result.stderr.includes("separate-command-stderr"));
}
const incomplete = await run(password, false);
assert.equal(incomplete.code, 1, incomplete.stderr);
assert(incomplete.stderr.includes("EOF"), incomplete.stderr);
