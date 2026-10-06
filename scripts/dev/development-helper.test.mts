import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { chmod, lstat, mkdir, mkdtemp, readFile, readdir, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test, { type TestContext } from "node:test";
import type { DevelopmentHelperManifest } from "./ctl-signed.mts";
import { publishDevelopmentHelper, type DevelopmentHelperSelection } from "./development-helper.mts";

async function fixture(t: TestContext) {
  const root = await mkdtemp(join(tmpdir(), "ctl-development-helper-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const repository_root = join(root, "repository");
  const target_directory = join(root, "shared Cargo target");
  const app = join(root, "signed/ctld.app");
  await mkdir(repository_root);
  await mkdir(join(target_directory, "ctl-dev"), { recursive: true, mode: 0o700 });
  await mkdir(join(app, "Contents/MacOS"), { recursive: true });
  await mkdir(join(app, "Contents/_CodeSignature"));
  await writeFile(join(app, "Contents/MacOS/ctld"), "signed helper executable", { mode: 0o755 });
  await writeFile(join(app, "Contents/embedded.provisionprofile"), "provisioning profile");
  await writeFile(join(app, "Contents/Info.plist"), "signed application metadata");
  await writeFile(join(app, "Contents/_CodeSignature/CodeResources"), "signed resources");
  const sha256 = "a".repeat(64);
  const manifest: DevelopmentHelperManifest = {
    schema_version: 1, component: "ctld", app_version: "0.1.0", bundle_id: `dev.${sha256}`,
    git_revision: "b".repeat(40), target: "aarch64-apple-darwin", bundle_identifier: "dev.tokn-ai.ctl.ctld",
    team_identifier: "TEAM123ABC", signing_mode: "development", notarized: false,
    archive: "ctld-0.1.0-aarch64-apple-darwin.app.tar.gz", sha256, archive_size: 1024,
    development: { source_fingerprint: "c".repeat(64), dirty: true },
    protocols: ["ctld", "ctld_lifecycle", "ctld_helper"].map((name) => ({
      name, version: "1.0.1", build: 1, supported_versions: ["1.0.1"],
    })),
  };
  const identity = createHash("sha256").update(await realpath(repository_root), "utf8").digest("hex").slice(0, 20);
  const directory = join(target_directory, "ctl-dev/helpers", identity);
  const selected = join(directory, "selected.json");
  const checks: string[] = [];
  const verify = async (app: string, receipt: DevelopmentHelperManifest) => {
    checks.push(app);
    assert.deepEqual(receipt, manifest);
    assert.equal(await readFile(join(app, "Contents/MacOS/ctld"), "utf8"), "signed helper executable");
  };
  return { root, repository_root, target_directory, app, manifest, directory, selected, checks, verify };
}

test("publication uses a canonical worktree checkpoint and exposes only a complete private signed bundle", async (t) => {
  const input = await fixture(t);
  const alias = join(input.root, "repository-alias");
  await symlink(input.repository_root, alias);
  let verified = false;
  const helper = await publishDevelopmentHelper({ ...input, repository_root: alias }, async (app, manifest) => {
    await assert.rejects(readFile(input.selected), { code: "ENOENT" });
    await assert.rejects(lstat(join(input.directory, `build-${manifest.sha256}`)), { code: "ENOENT" });
    await input.verify(app, manifest);
    verified = true;
  });
  assert.equal(verified, true);
  const selection: DevelopmentHelperSelection = JSON.parse(await readFile(input.selected, "utf8"));
  assert.deepEqual(selection, {
    schema_version: 1, repository_root: await realpath(input.repository_root), target: input.manifest.target,
    directory: `build-${input.manifest.sha256}`,
  });
  const published = join(input.directory, selection.directory);
  assert.equal(helper, join(published, "ctld.app/Contents/MacOS/ctld"));
  assert.deepEqual(JSON.parse(await readFile(join(published, "ctld-package.json"), "utf8")), input.manifest);
  for (const directory of [dirname(input.directory), input.directory, published, join(published, "ctld.app/Contents")]) {
    assert.equal((await lstat(directory)).mode & 0o777, 0o700);
  }
  for (const file of [input.selected, join(published, "ctld-package.json"), join(published, "ctld.app/Contents/embedded.provisionprofile")]) {
    assert.equal((await lstat(file)).mode & 0o777, 0o600);
  }
  assert.equal((await lstat(helper)).mode & 0o777, 0o755);
  assert.deepEqual((await readdir(input.directory)).sort(), [`build-${input.manifest.sha256}`, "selected.json"]);
});

test("verified cached publication retains the same helper inode and checks its signature again", async (t) => {
  const input = await fixture(t);
  const helper = await publishDevelopmentHelper(input, input.verify);
  const before = await lstat(helper);
  assert.equal(await publishDevelopmentHelper(input, input.verify), helper);
  const after = await lstat(helper);
  assert.equal(after.ino, before.ino);
  assert.equal(after.mtimeMs, before.mtimeMs);
  assert.equal(input.checks.length, 2);
  assert.equal(input.checks[1], dirname(dirname(dirname(helper))));
});

test("verification failure preserves the selected checkpoint and removes unpublished staging", async (t) => {
  const input = await fixture(t);
  const helper = await publishDevelopmentHelper(input, input.verify);
  const previous = await readFile(input.selected);
  const sha256 = "d".repeat(64);
  const manifest = { ...input.manifest, bundle_id: `dev.${sha256}`, sha256 };
  await assert.rejects(publishDevelopmentHelper({ ...input, manifest }, async () => {
    assert.deepEqual(await readFile(input.selected), previous);
    throw new Error("fixture signature verification failed");
  }), /signature verification failed/);
  assert.deepEqual(await readFile(input.selected), previous);
  assert.equal(await readFile(helper, "utf8"), "signed helper executable");
  assert.deepEqual((await readdir(input.directory)).sort(), [`build-${input.manifest.sha256}`, "selected.json"]);
});

for (const corruption of ["receipt", "contents", "permissions", "signature", "extra-file"] as const) {
  test(`a cached helper with invalid ${corruption} is never overwritten or selected`, async (t) => {
    const input = await fixture(t);
    const helper = await publishDevelopmentHelper(input, input.verify);
    const previous = await readFile(input.selected);
    const published = join(input.directory, `build-${input.manifest.sha256}`);
    if (corruption === "receipt") await writeFile(join(published, "ctld-package.json"), JSON.stringify({ ...input.manifest, team_identifier: "OTHER123AB" }));
    if (corruption === "contents") await writeFile(helper, "damaged live helper");
    if (corruption === "permissions") await chmod(join(published, "ctld.app/Contents"), 0o755);
    if (corruption === "extra-file") await writeFile(join(published, "unexpected"), "untrusted extra file");
    const before = await lstat(helper);
    await assert.rejects(publishDevelopmentHelper(input, corruption === "signature" ? async () => {
      throw new Error("fixture signature no longer valid");
    } : input.verify));
    assert.deepEqual(await readFile(input.selected), previous);
    assert.equal((await lstat(helper)).ino, before.ino);
    assert.equal(await readFile(helper, "utf8"), corruption === "contents" ? "damaged live helper" : "signed helper executable");
  });
}

test("incomplete existing builds fail without recreating or overwriting files", async (t) => {
  const input = await fixture(t);
  await publishDevelopmentHelper(input, input.verify);
  const previous = await readFile(input.selected);
  await rm(join(input.directory, `build-${input.manifest.sha256}`, "ctld-package.json"));
  await assert.rejects(publishDevelopmentHelper(input, input.verify), /incomplete/);
  assert.deepEqual(await readFile(input.selected), previous);
  await assert.rejects(readFile(join(input.directory, `build-${input.manifest.sha256}`, "ctld-package.json")), { code: "ENOENT" });
});

test("worktrees sharing a custom Cargo target keep independent helper selections", async (t) => {
  const input = await fixture(t);
  const helper = await publishDevelopmentHelper(input, input.verify);
  const other_root = join(input.root, "other-worktree");
  await mkdir(other_root);
  const other = await publishDevelopmentHelper({ ...input, repository_root: other_root }, input.verify);
  assert.notEqual(other, helper);
  assert.equal(await readFile(helper, "utf8"), await readFile(other, "utf8"));
  const directories = await readdir(join(input.target_directory, "ctl-dev/helpers"));
  assert.equal(directories.length, 2);
  for (const directory of directories) {
    const selection: DevelopmentHelperSelection = JSON.parse(await readFile(join(input.target_directory, "ctl-dev/helpers", directory, "selected.json"), "utf8"));
    assert.ok([await realpath(input.repository_root), await realpath(other_root)].includes(selection.repository_root));
  }
});
