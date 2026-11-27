import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { before, type TestContext } from "node:test";
import { fileURLToPath } from "node:url";
import { Ctream } from "@tokn-ai/ctream-sdk";
import { isScreenshot } from "../src/model.ts";
import { SessionTransport, type Request } from "../src/session-transport.ts";

const requestTimeoutMs = 10_000;
let executable: string;

before(() => {
  const manifest = fileURLToPath(new URL("../../cli/Cargo.toml", import.meta.url));
  const output = execFileSync("cargo", [
    "build", "--locked", "--manifest-path", manifest, "--bin", "ctream", "--message-format=json-render-diagnostics",
  ], {encoding: "utf8", stdio: ["ignore", "pipe", "inherit"]});
  const artifact = output.trim().split("\n").map(line => JSON.parse(line))
    .find(message => message.reason === "compiler-artifact" && message.target.name === "ctream" && message.executable);
  assert.ok(artifact, "Cargo must report the ctream executable");
  executable = artifact.executable;
}, {timeout: 120_000}); // A cold Rust build can exceed Bun's 5s hook default.

function fixture(t: TestContext, profile: "current" | "legacy" | "denied", turns = false) {
  const dir = mkdtempSync(join(tmpdir(), "ctream-native-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const launcher = fileURLToPath(new URL("./ssh-fixture.mts", import.meta.url));
  const sshArgs = [launcher, dir, profile];
  const client = new Ctream({
    executable,
    host: "fixture",
    agent: "ctream-agent",
    ssh_program: process.execPath,
    ssh_args: sshArgs,
    timeout_ms: requestTimeoutMs,
    turns,
  });
  const read = (name: string) => readFileSync(join(dir, name), "utf8");
  function legacySession() {
    const args = ["--host", "fixture", "--agent", "ctream-agent", "--ssh-program", process.execPath,
      ...sshArgs.flatMap(arg => ["--ssh-arg", arg]), "session", ":2", "--window", "x11-7", "--protocol", "1.0.2"];
    const transport = new SessionTransport(() => spawn(executable, args), requestTimeoutMs);
    return {
      screenshot: () => transport.request({operation: "screenshot"}, value => {
        assert.ok(isScreenshot(value));
        return value;
      }),
      act: (request: Request) => transport.request(request, value => assert.equal(value, null)),
      close: () => transport.close(),
    };
  }
  return { client, read, legacySession };
}

test("watch negotiates the live host protocol over SSH and leaves the selected window on break", async (t) => {
  const {client, read} = fixture(t, "current");
  const watch = client.watch(":2", "x11-7");
  const timeout = setTimeout(() => { watch.close().catch(() => {}); }, requestTimeoutMs);
  try {
    let roster = false;
    for await (const event of watch) {
      assert.ok(event.type === "snapshot");
      assert.deepEqual(event.participants, [{participant_id: event.self_id, agent_id: client.agent_id}]);
      roster = true;
      break;
    }
    assert.ok(roster);
    assert.deepEqual(read("lifecycle").trim().split("\n").map(line => line.split(" ").slice(1).join(" ")), [
      "watch open", "watch close",
    ]);
  } finally {
    clearTimeout(timeout);
    await watch.close();
  }
});

test("opted-in sessions and watches negotiate turn protocols over SSH", async (t) => {
  const {client, read} = fixture(t, "current", true);
  const session = client.session(":2", "x11-7");
  const watch = client.watch(":2", "x11-7");
  const timeout = setTimeout(() => { watch.close().catch(() => {}); }, requestTimeoutMs);
  try {
    await session.withTurn(turn => turn.pressKeys("Tab"));
    assert.equal((await watch.next()).value?.type, "snapshot");
    assert.deepEqual((await watch.next()).value, {type: "turn", holder: null, expires_at_ms: null});
  } finally {
    clearTimeout(timeout);
    await Promise.all([session.close(), watch.close()]);
  }
  assert.deepEqual(read("lifecycle").trim().split("\n").map(line => line.split(" ")[1]), ["turn-session", "turn-room"]);
  assert.deepEqual(read("events").trim().split("\n").map(line => JSON.parse(line)), [
    {operation: "acquire_turn"}, {operation: "press_keys", keys: ["Tab"], turn_id: "fixture-turn"},
    {operation: "release_turn", turn_id: "fixture-turn"},
  ]);
});

test("refused turn negotiation never retries through an unprotected session", async (t) => {
  const {client, read} = fixture(t, "legacy", true);
  const session = client.session(":2", "x11-7");
  await assert.rejects(session.withTurn(() => assert.fail("unexpected turn")), /unsupported control turn protocol/);
  await assert.rejects(session.close(), /unsupported control turn protocol/);
  assert.match(read("lifecycle"), /^\d+ turn-session\n$/);
});

test("the SDK sends its generated ID to the display host across sessions", async (t) => {
  const { client, read } = fixture(t, "current");
  const [display] = await client.displays();
  assert.ok(display.target);
  const window = await client.resolveWindow(display.target, { title: "Editor" });
  const second = client.session(display.target, "x11-7");
  try {
    await window.capabilities();
    await window.pressKeys("Return");
    await second.pressKeys("Tab");
  } finally {
    await Promise.all([window.close(), second.close()]);
  }
  assert.match(client.agent_id, /^[a-z]+#[a-f0-9]{4}$/);
  assert.deepEqual(read("events").trim().split("\n"), [
    `${client.agent_id} Return`, `${client.agent_id} Tab`,
  ]);
  assert.deepEqual(read("lifecycle").trim().split("\n").map(line => line.split(" ").slice(1).join(" ")), [
    "attributed open", "attributed open", "attributed close", "attributed close",
  ]);
});

test("a host that rejects a generated ID receives no fallback input", async (t) => {
  const { client, read } = fixture(t, "legacy");
  const window = client.session(":2", "x11-7");
  await assert.rejects(window.pressKeys("Tab"), /unsupported attributed-session version/);
  await assert.rejects(window.close(), /unsupported attributed-session version/);
  assert.match(read("lifecycle"), /^\d+ attributed open\n$/);
});

for (const profile of ["current", "legacy"] as const) {
  test(`CLI protocol 1.0.2 reuses capture and confirms input on a ${profile} agent`, async (t) => {
    const { legacySession, read } = fixture(t, profile);
    const window = legacySession();
    try {
      const first = await window.screenshot();
      assert.deepEqual([first.width, first.height], [2, 1]);
      assert.deepEqual(Buffer.from(first.data, "base64").subarray(0, 8), Buffer.from("89504e470d0a1a0a", "hex"));
      await window.act({operation: "click", point: {x: 1, y: 0}, size: [first.width, first.height], button: "left"});
      const after = await window.screenshot();
      assert.deepEqual([after.width, after.height], [1, 1]);
      assert.notEqual(after.data, first.data);
      assert.equal((await window.screenshot()).data, after.data);
      await window.act({operation: "press_keys", keys: ["Return"]});
      assert.equal(read("events"), "move 1 0 2 1\nbutton 1 1\nbutton 1 0\nkey Return 1\nkey Return 0\n");
    } finally {
      await window.close();
    }

    const lifecycle = read("lifecycle").trim().split("\n");
    assert.equal(new Set(lifecycle.map(line => line.split(" ")[0])).size, 1, "one local CLI owns all channels");
    assert.deepEqual(lifecycle.map(line => line.slice(line.indexOf(" ") + 1)), [
      "capture open", "probe",
      ...(profile === "current" ? ["input open", "input close"] : ["legacy open", "legacy close", "legacy open", "legacy close"]),
      "capture close",
    ]);
  });

  test(`CLI protocol 1.0.2 propagates a late ${profile} input failure without replay`, async (t) => {
    const { legacySession, read } = fixture(t, profile);
    const window = legacySession();
    try {
      await window.act({operation: "press_keys", keys: ["Return"]});
      const tab = {operation: "press_keys", keys: ["Tab"]};
      await assert.rejects(window.act(tab), /window disappeared/);
      await assert.rejects(window.act(tab));
      assert.equal(read("events").split("key Tab 1").length - 1, 1);
    } finally {
      await window.close();
    }
  });
}

test("a denied input probe in CLI protocol 1.0.2 never opens an input channel", async (t) => {
  const { legacySession, read } = fixture(t, "denied");
  const window = legacySession();
  try {
    await assert.rejects(window.act({operation: "press_keys", keys: ["Tab"]}), /agent access denied/);
    assert.match(read("lifecycle"), /^\d+ probe\n$/);
  } finally {
    await window.close();
  }
});
