import { spawn } from "node:child_process";
import { connect } from "node:net";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";

const ctl = process.argv[2];
const password = "  caller-credential-☃-canary  ";
const input = "command stdin remains independent\n";

async function run(payload: string, close = true, plain = false) {
  const windows = process.platform === "win32";
  const pipe = `\\\\.\\pipe\\ctl-password-${randomUUID()}`;
  const options = windows ? ["--password-pipe", pipe] : ["--password-fd", "3"];
  const child = spawn(ctl, ["--destination", "caller@[::1]", ...options,
    ...(plain ? ["shell", "--plain"] : ["exec", "--", "cat"])], {
    stdio: windows ? ["pipe", "pipe", "pipe"] : ["pipe", "pipe", "pipe", "pipe"],
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
  if (windows) {
    const deadline = Date.now() + 4000;
    while (true) {
      try {
        channel = await new Promise<import("node:net").Socket>((resolve, reject) => {
          const socket = connect(pipe);
          socket.once("error", reject);
          socket.once("connect", () => { socket.removeListener("error", reject); resolve(socket); });
        });
        break;
      } catch (error) {
        if (Date.now() >= deadline) throw error;
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
    }
  } else {
    channel = child.stdio[3] as import("node:stream").Writable;
  }
  channel.on("error", () => {});
  channel.write(Buffer.from(payload));
  if (close) channel.end();
  if (close) child.stdin!.end(input);
  else child.stdin!.write(input);
  const code = await exited;
  channel.destroy();
  clearTimeout(timer);
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
for (const payload of ["", "bad\npassword", "x".repeat(4097), "bad\u0000password"]) {
  const result = await run(payload);
  assert.equal(result.code, 1, result.stderr);
  assert(!result.stderr.includes("separate-command-stderr"));
}
const incomplete = await run(password, false);
assert.equal(incomplete.code, 1, incomplete.stderr);
assert(incomplete.stderr.includes("EOF"), incomplete.stderr);
