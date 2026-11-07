import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { componentSetFixture, publicationFixture } from "../ci/bundle-publication.fixtures.mts";

test("sync stages verified desktop resources without invoking Cargo or changing account state", { skip: process.platform === "win32" }, async (t) => {
  const root = await mkdtemp(join(tmpdir(), "ctl-sync-resources-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const home = join(root, "home"), binaries = join(root, "bin"), repository = join(root, "repository");
  for (const path of [home, binaries, repository]) await mkdir(path);
  const publication = publicationFixture(1);
  const artifacts = await componentSetFixture(root, publication);
  const config_directory = join(repository, "apps/desktop/src-tauri");
  await mkdir(config_directory, { recursive: true });
  await writeFile(join(config_directory, "tauri.conf.json"), JSON.stringify({ version: publication.app_version }));
  const scripts = {
    git: "console.log(process.env.CTL_SYNC_TEST_REPOSITORY);",
    gh: `import {cpSync} from 'node:fs';
const args=process.argv.slice(2), revision=process.env.CTL_SYNC_TEST_REVISION;
if(args[0]==='run'&&args[1]==='list') console.log(JSON.stringify([{databaseId:1,conclusion:'success',status:'completed',headSha:revision,event:'push',createdAt:new Date().toISOString()}]));
else if(args[0]==='run'&&args[1]==='view') console.log(JSON.stringify({conclusion:'success',status:'completed',headSha:revision}));
else if(args[0]==='run'&&args[1]==='download') cpSync(process.env.CTL_SYNC_TEST_ARTIFACTS,args[args.indexOf('--dir')+1],{recursive:true});
else process.exit(99);`,
    cargo: "throw new Error('Resource staging must not invoke Cargo or an installer');",
  };
  for (const [name, body] of Object.entries(scripts)) {
    const path = join(binaries, name);
    await writeFile(path, `#!/usr/bin/env node\n${body}\n`);
    await chmod(path, 0o700);
  }
  // The wrappers contain ESM imports but intentionally have normal executable
  // names so spawnSync resolves them exactly like real git/gh/Cargo commands.
  await writeFile(join(binaries, "package.json"), '{"type":"module"}');
  const { stdout } = await promisify(execFile)(process.execPath, ["--experimental-strip-types", "--disable-warning=ExperimentalWarning",
    fileURLToPath(new URL("./sync-agent-bundles.mts", import.meta.url)), "--main"], {
    cwd: repository,
    env: { ...process.env, HOME: home, PATH: `${binaries}${delimiter}${process.env.PATH ?? ""}`,
      CTL_SYNC_TEST_REPOSITORY: repository, CTL_SYNC_TEST_ARTIFACTS: artifacts, CTL_SYNC_TEST_REVISION: publication.git_revision },
  });
  const resources = join(config_directory, "resources/agent-bundles");
  assert.deepEqual((await readdir(resources)).sort(), (await readdir(artifacts)).sort());
  assert.deepEqual(await readFile(join(resources, "bundle-set.json")), await readFile(join(artifacts, "bundle-set.json")));
  assert.deepEqual(await readdir(home), []);
  assert.match(stdout, /Staged verified packaging resources/);
});
