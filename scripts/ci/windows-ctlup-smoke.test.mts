import assert from "node:assert/strict";
import test from "node:test";
import { json } from "./windows-ctlup-smoke.mts";

test("the Windows ctlup fixture drains large progress output without interrupting JSON commands", async () => {
  const result = await json(process.execPath, ["-e", `
    process.stderr.write('progress '.repeat(200_000));
    process.stdout.write(JSON.stringify({ installed: true }));
  `]);
  assert.deepEqual(result, { installed: true });
});

test("the Windows ctlup fixture retains the final failure diagnostic and bounds JSON output", async () => {
  await assert.rejects(json(process.execPath, ["-e", `
    process.stderr.write('old progress '.repeat(100_000));
    process.stderr.write('Final bootstrap failure');
    process.exitCode = 17;
  `]), (error: Error) => {
    assert.match(error.message, /Command exited 17/);
    assert.match(error.message, /Final bootstrap failure$/);
    assert.ok(error.message.length < 34 * 1024);
    return true;
  });
  await assert.rejects(json(process.execPath, ["-e", "process.stdout.write('x'.repeat(2 * 1024 * 1024))"]), /JSON exceeded/);
});

test("the Windows ctlup fixture terminates commands that exceed its deadline", async () => {
  await assert.rejects(json(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { timeout_ms: 50 }), /Command timed out/);
});
