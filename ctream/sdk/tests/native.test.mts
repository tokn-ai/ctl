import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { Ctream } from "@tokn-ai/ctream-sdk";

test("the SDK drives the native CLI and reports an agent failure after input EOF", async (t) => {
  const dir = mkdtempSync(join(tmpdir(), "ctream-native-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const log = join(dir, "events");
  const manifest = fileURLToPath(new URL("../../cli/Cargo.toml", import.meta.url));
  const launcher = fileURLToPath(new URL("./ssh-fixture.mts", import.meta.url));
  const client = new Ctream({
    executable: ["cargo", "run", "--quiet", "--locked", "--manifest-path", manifest, "--"],
    host: "fixture",
    agent: "ctream-agent",
    ssh_program: process.execPath,
    ssh_args: [launcher, log],
  });

  const displays = await client.displays();
  const target = displays[0].target;
  assert.ok(target);
  const window = await client.resolveWindow(target, { title: "Editor" });
  const frame = await window.screenshot();
  assert.deepEqual([frame.width, frame.height], [2, 1]);
  assert.deepEqual(Buffer.from(frame.data, "base64").subarray(0, 8), Buffer.from("89504e470d0a1a0a", "hex"));
  await window.click({ x: 1, y: 0 });
  assert.equal(readFileSync(log, "utf8"), "move 1 0 2 1\nbutton 1 1\nbutton 1 0\n");
  await assert.rejects(window.pressKeys("Tab"), /window disappeared/);
});
