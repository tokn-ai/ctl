import assert from "node:assert/strict";
import { chmod, mkdir, mkdtemp, readFile, rm, stat, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { createServer } from "node:net";
import path from "node:path";
import { test, type TestContext } from "node:test";
import { createSignedSupervisorDirectory, prepareSignedRuntime } from "./signed-runtime.mts";
import { requestPreparation, servePreparation } from "./daemon-preparation.mts";

async function fixture(context: TestContext) {
  const root = await mkdtemp(path.join(tmpdir(), "signed-runtime-test-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  const repository = path.join(root, "worktree");
  await mkdir(repository);
  return { root, repository };
}

test("separate launches reuse the same private runtime without removing its contents", async (context) => {
  const { root, repository } = await fixture(context);
  const first = await prepareSignedRuntime(repository, root);
  await writeFile(path.join(first, "owner-fixture"), "keep");
  const next = await prepareSignedRuntime(repository, root);
  assert.equal(next, first);
  assert.equal(await readFile(path.join(next, "owner-fixture"), "utf8"), "keep");
  if (process.platform !== "win32") assert.equal((await stat(next)).mode & 0o777, 0o700);
});

test("different worktrees have independent endpoints", async (context) => {
  const { root, repository } = await fixture(context);
  const sibling = path.join(root, "other-worktree");
  await mkdir(sibling);
  assert.notEqual(await prepareSignedRuntime(repository, root), await prepareSignedRuntime(sibling, root));
});

test("the system runtime fits a Unix socket even for a long worktree path", { skip: process.platform === "win32" }, async (context) => {
  const { repository } = await fixture(context);
  const nested = path.join(repository, "long-worktree-directory-".repeat(5));
  await mkdir(nested);
  const runtime = await prepareSignedRuntime(nested);
  context.after(() => rm(runtime, { recursive: true, force: true }));
  const server = createServer();
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(path.join(runtime, "ctld.sock"), resolve);
  });
  await new Promise<void>((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
});

test("a symlinked worktree keeps its original endpoint", { skip: process.platform === "win32" }, async (context) => {
  const { root, repository } = await fixture(context);
  const alias = path.join(root, "worktree-alias");
  await symlink(repository, alias);
  assert.equal(await prepareSignedRuntime(repository, root), await prepareSignedRuntime(alias, root));
});

test("concurrent launchers have independent preparation sockets on one runtime", { skip: process.platform === "win32" }, async (context) => {
  const { repository } = await fixture(context);
  const runtime = await prepareSignedRuntime(repository);
  context.after(() => rm(runtime, { recursive: true, force: true }));
  const first = await createSignedSupervisorDirectory(runtime);
  const second = await createSignedSupervisorDirectory(runtime);
  assert.notEqual(first, second);
  const seen: string[] = [];
  const first_socket = path.join(first, "prepare.sock");
  const second_socket = path.join(second, "prepare.sock");
  const first_server = await servePreparation(first_socket, async () => { seen.push("first"); });
  context.after(() => first_server.close());
  const second_server = await servePreparation(second_socket, async () => { seen.push("second"); });
  context.after(() => second_server.close());
  await requestPreparation(first_socket, "fixture");
  await requestPreparation(second_socket, "fixture");
  assert.deepEqual(seen, ["first", "second"]);
});

test("rejects a replaced runtime symlink without modifying its target", { skip: process.platform === "win32" }, async (context) => {
  const { root, repository } = await fixture(context);
  const runtime = await prepareSignedRuntime(repository, root);
  await rm(runtime, { recursive: true });
  const unrelated = path.join(root, "unrelated");
  await mkdir(unrelated, { mode: 0o700 });
  await writeFile(path.join(unrelated, "fixture"), "keep");
  await symlink(unrelated, runtime);
  await assert.rejects(prepareSignedRuntime(repository, root), /owner-only directory/);
  assert.equal(await readFile(path.join(unrelated, "fixture"), "utf8"), "keep");
});

test("rejects an existing runtime accessible to other users", { skip: process.platform === "win32" }, async (context) => {
  const { root, repository } = await fixture(context);
  const runtime = await prepareSignedRuntime(repository, root);
  await chmod(runtime, 0o755);
  await assert.rejects(prepareSignedRuntime(repository, root), /owner-only directory/);
});
