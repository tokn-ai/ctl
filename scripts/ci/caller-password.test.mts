import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { Writable } from "node:stream";
import test from "node:test";
import { endAndClose } from "./caller-password.mts";

test("the caller fixture flushes a pipe before explicitly closing non-auto-destroy sinks", async () => {
  const operations: string[] = [];
  let received = "";
  const stream = new Writable({
    autoDestroy: false,
    write(chunk: Buffer, _encoding, callback) {
      operations.push("write");
      setTimeout(() => {
        received += chunk.toString();
        operations.push("flushed");
        callback();
      }, 10);
    },
    destroy(error, callback) {
      operations.push("closed");
      callback(error);
    },
  });
  await endAndClose(stream, "command stdin\r\n");
  assert.equal(received, "command stdin\r\n");
  assert.deepEqual(operations, ["write", "flushed", "closed"]);
  assert.equal(stream.destroyed, true);
});

test("the caller fixture reports a failed write instead of awaiting child exit", async () => {
  const stream = new Writable({ write(_chunk, _encoding, callback) { callback(new Error("write failed")); } });
  await assert.rejects(endAndClose(stream, "ordinary input"), /write failed/);
});

test("the caller fixture delivers stdin EOF before waiting for a real child to exit", { timeout: 5000 }, async () => {
  const child = spawn(process.execPath, ["-e", `
    let input = '';
    process.stdin.on('data', (bytes) => { input += bytes.toString(); });
    process.stdin.on('end', () => { process.stdout.write(input); });
  `], { stdio: ["pipe", "pipe", "pipe"] });
  let output = "";
  child.stdout.on("data", (bytes) => { output += bytes.toString(); });
  const exited = new Promise<number | null>((resolve, reject) => {
    child.once("error", reject);
    child.once("close", resolve);
  });
  try {
    await endAndClose(child.stdin, "ordinary command stdin\r\n");
    assert.equal(await exited, 0);
    assert.equal(output, "ordinary command stdin\r\n");
  } finally {
    child.kill();
    child.stdin.destroy();
    child.stdout.destroy();
    child.stderr.destroy();
  }
});
