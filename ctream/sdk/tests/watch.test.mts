import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, {type TestContext} from "node:test";
import { fileURLToPath } from "node:url";
import { Ctream, type LiveEvent } from "@tokn-ai/ctream-sdk";

const fixture = fileURLToPath(new URL("./watch-fixture.mts", import.meta.url));

function setup(t: TestContext, mode: string, turns = false) {
  const dir = mkdtempSync(join(tmpdir(), "ctream-watch-"));
  const log = join(dir, "watch");
  t.after(() => rmSync(dir, {recursive: true, force: true}));
  const client = new Ctream({
    executable: [process.execPath, fixture, log, mode], ssh_args: ["-o", "A B"],
    host: "display-host", agent_id: "otter#abcd", turns,
  });
  const calls = () => existsSync(log) ? readFileSync(log, "utf8").trim().split("\n").map((v) => JSON.parse(v)) : [];
  return {client, calls};
}

test("watch reads a fragmented snapshot and batched events in order until clean EOF", async (t) => {
  const {client, calls} = setup(t, "batch");
  const watch = client.watch(":1", "window id");
  assert.deepEqual(calls(), []);
  const events: LiveEvent[] = [];
  for await (const event of watch) events.push(event);
  const {started, args} = calls()[0];
  assert.deepEqual(args, ["--host", "display-host", "--ssh-arg", "-o", "--ssh-arg", "A B",
    "watch", ":1", "--agent-id", "otter#abcd", "--window", "window id", "--protocol", "1.0.5"]);
  const peer = {participant_id: "other-session", agent_id: "writer"};
  assert.deepEqual(events, [
    {type: "snapshot", self_id: started, participants: [{participant_id: started, agent_id: client.agent_id}]},
    {type: "joined", participant: peer},
    {type: "action", participant_id: peer.participant_id, action: {operation: "click", point: [0, 1]}},
    {type: "action", participant_id: peer.participant_id, action: {operation: "drag", from: [0.1, 0.3], to: [0.9, 0.2]}},
    {type: "action", participant_id: peer.participant_id, action: {operation: "type_text"}},
    {type: "left", participant_id: peer.participant_id},
  ]);
  await watch.close();
});

test("breaking a watch leaves its participant and keeps another watch open", async (t) => {
  const {client, calls} = setup(t, "quiet");
  const first = client.watch(":1");
  const other = client.watch(":1");
  try {
    const {value: snapshot} = await other.next();
    assert.ok(snapshot?.type === "snapshot");
    for await (const event of first) {
      assert.ok(event.type === "snapshot" && event.self_id !== snapshot.self_id);
      break;
    }
    assert.deepEqual(calls().map((call) => Object.keys(call)[0]), ["started", "started", "left"]);
    assert.equal(calls()[2].left, calls()[1].started);
    const pending = other.next();
    await assert.rejects(other.next(), /pending read/);
    await Promise.all([other.close(), other[Symbol.asyncDispose]()]);
    assert.deepEqual(await pending, {done: true, value: undefined});
    assert.equal(calls()[3].left, snapshot.self_id);
  } finally {
    await Promise.all([first.close(), other.close()]);
  }
});

test("an opted-in watch receives the desktop turn, including a holder outside the source roster", async (t) => {
  const {client, calls} = setup(t, "turns", true);
  const events: LiveEvent[] = [];
  for await (const event of client.watch(":1")) events.push(event);
  assert.deepEqual(events.map(event => event.type), ["snapshot", "turn", "joined", "action", "turn"]);
  assert.deepEqual(events[1], {type: "turn", holder: {participant_id: "other-session", agent_id: "writer"}, expires_at_ms: 12345});
  assert.deepEqual(events[3], {type: "action", participant_id: "other-session", action: {operation: "press_keys"},
    action_id: "12345678-1234-1234-1234-123456789abc"});
  assert.deepEqual(events[4], {type: "turn", holder: null, expires_at_ms: null});
  assert.deepEqual(calls()[0].args.slice(-2), ["--protocol", "1.0.7"]);
});

test("an opted-in watch rejects a missing initial control turn", async (t) => {
  const {client} = setup(t, "batch", true);
  const watch = client.watch(":1");
  assert.equal((await watch.next()).value?.type, "snapshot");
  await assert.rejects(watch.next(), /expected initial control turn/);
  await watch.close();
});

test("closing before iteration never starts a watch", async (t) => {
  const {client, calls} = setup(t, "quiet");
  const watch = client.watch(":1");
  await watch.close();
  assert.equal((await watch.next()).done, true);
  assert.deepEqual(calls(), []);
});

test("watch accepts the 64 KiB record boundary excluding the newline", async (t) => {
  const {client} = setup(t, "limit");
  const watch = client.watch(":1");
  try {
    assert.equal((await watch.next()).value?.type, "snapshot");
    assert.equal((await watch.next()).done, true);
  } finally {
    await watch.close();
  }
});

test("watch reports an unavailable CLI on the first read", async () => {
  const watch = new Ctream({executable: "ctream-missing-watch-test-binary"}).watch(":1");
  await assert.rejects(watch.next(), /ENOENT|Executable not found/);
  await watch.close().catch(() => {});
});

for (const [mode, expected] of Object.entries({
  overflow: /exceeds 64 KiB/, truncated: /truncated/, invalid_json: /invalid JSON/,
  wrong_first: /initial.*snapshot/, missing_first: /before its initial snapshot/, wrong_point: /coordinates/,
  reject: /unsupported on this display host/, exit: /display host disconnected/, duplicate: /invalid ctream watch event/,
})) {
  test(`watch rejects ${mode} without replay`, async (t) => {
    const {client, calls} = setup(t, mode);
    const watch = client.watch(":1");
    await assert.rejects(async () => { for await (const _event of watch) {} }, expected, mode);
    assert.equal(calls().filter((call) => call.started).length, 1);
    await watch.close().catch(() => {});
  });
}

test("a stalled consumer applies backpressure or disconnects when its bounded buffer fills", async (t) => {
  const {client, calls} = setup(t, "flood");
  const watch = client.watch(":1");
  try {
    assert.equal((await watch.next()).value?.type, "snapshot");
    if (process.versions.bun) {
      // Bun drains its native pipe eagerly. Wait for the SDK to release the
      // observer, then verify the error without consuming the buffered events.
      for (let i = 0; i < 100 && !calls().some((call) => call.left); i++) {
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      assert.ok(calls().some((call) => call.left));
      await assert.rejects(watch.close(), /1 MiB backlog/);
    } else {
      await new Promise((resolve) => setTimeout(resolve, 80));
      assert.equal(calls().some((call) => call.sent_all), false);
      await watch.close();
    }
  } finally {
    await watch.close().catch(() => {});
  }
  assert.ok(calls().some((call) => call.left));
});
