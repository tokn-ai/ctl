import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdir, mkdtemp, readdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { test, type TestContext } from "node:test";
import { withPreparationLock } from "./signed-preparation-lock.mts";

const unixOnly = { skip: process.platform === "win32" };

async function fixture(context: TestContext): Promise<string> {
  const directory = await mkdtemp(path.join(tmpdir(), "signed-lock-test-"));
  context.after(() => rm(directory, { recursive: true, force: true }));
  return directory;
}

test("concurrent preparations serialize across repeated lock handoffs", unixOnly, async (context) => {
  const directory = await fixture(context);
  let active = 0;
  let completed = 0;
  await Promise.all(Array.from({ length: 32 }, () => withPreparationLock(directory, async () => {
    active += 1;
    assert.equal(active, 1);
    // Yield while holding the lock so contenders also observe release races.
    await new Promise((resolve) => setImmediate(resolve));
    completed += 1;
    active -= 1;
  }, 5_000)));
  assert.equal(completed, 32);
  await assert.rejects(stat(path.join(directory, "prepare.lock")), { code: "ENOENT" });
});

test("a failed preparation releases its lock for the next caller", unixOnly, async (context) => {
  const directory = await fixture(context);
  await assert.rejects(withPreparationLock(directory, async () => {
    throw new Error("fixture signing failure");
  }), /fixture signing failure/);
  assert.equal(await withPreparationLock(directory, async () => "retry"), "retry");
});

test("recovers a lock whose recorded owner process has exited", unixOnly, async (context) => {
  const directory = await fixture(context);
  const child = spawn(process.execPath, ["-e", ""], { stdio: "ignore" });
  await new Promise<void>((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", () => resolve());
  });
  assert.ok(child.pid);
  const lock = path.join(directory, "prepare.lock");
  await mkdir(lock, { mode: 0o700 });
  await writeFile(path.join(lock, `owner-${child.pid}-${randomUUID()}`), "", { mode: 0o600 });
  assert.equal(await withPreparationLock(directory, async () => "recovered"), "recovered");
  await assert.rejects(stat(lock), { code: "ENOENT" });
});

test("an incomplete lock times out without being removed or claimed", unixOnly, async (context) => {
  const directory = await fixture(context);
  const lock = path.join(directory, "prepare.lock");
  await mkdir(lock, { mode: 0o700 });
  let entered = false;
  await assert.rejects(withPreparationLock(directory, async () => { entered = true; }, 30), /busy or its lock is incomplete/);
  assert.equal(entered, false);
  assert.deepEqual(await readdir(lock), []);
});

test("an invalid owner record fails without modifying the lock", unixOnly, async (context) => {
  const directory = await fixture(context);
  const lock = path.join(directory, "prepare.lock");
  await mkdir(lock, { mode: 0o700 });
  await writeFile(path.join(lock, "unexpected"), "fixture", { mode: 0o600 });
  await assert.rejects(withPreparationLock(directory, async () => {}), /lock is invalid/);
  assert.deepEqual(await readdir(lock), ["unexpected"]);
});
