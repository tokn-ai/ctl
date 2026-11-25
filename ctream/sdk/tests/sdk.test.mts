import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { promisify } from "node:util";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, {type TestContext} from "node:test";
import { fileURLToPath } from "node:url";
import { Ctream, type CtreamOptions } from "@tokn-ai/ctream-sdk";

const fixture = fileURLToPath(new URL("./fixture.mts", import.meta.url));
function setup(t: TestContext, mode = "ok", options: CtreamOptions = {}) {
  const dir = mkdtempSync(join(tmpdir(), "ctream-sdk-"));
  const log = join(dir, "calls");
  t.after(() => rmSync(dir, {recursive: true, force: true}));
  const client = new Ctream({executable: [process.execPath, fixture, log, mode], ...options});
  const calls = () => readFileSync(log, "utf8").trimEnd().split("\n").map((line) => JSON.parse(line));
  const lifecycle = () => existsSync(log + ".lifecycle") ? readFileSync(log + ".lifecycle", "utf8").trim().split("\n") : [];
  return {client, calls, lifecycle, log};
}

test("lists displays and windows and preserves launcher arguments", async (t) => {
  const {client, calls} = setup(t, "ok", {
    host: "test-host", agent: "/some path/agent", ssh_program: "ssh launcher", ssh_args: ["-o", "argument ; with spaces"],
  });
  const displays = await client.displays();
  assert.deepEqual(displays.map(({target}) => target), [":3", "wayland-2", null, "windows-1", "macos"]);
  assert.equal(displays[2].selectable, false);
  const windows_display = displays[3].display;
  assert.ok(typeof windows_display === "object" && "windows" in windows_display);
  assert.deepEqual(windows_display.windows.bounds, [-1280, 0, 0, 720]);
  assert.ok(displays[0].target);
  assert.equal((await client.windows(displays[0].target))[0].id, "x11-1a");
  const prefix = ["--host", "test-host", "--agent", "/some path/agent", "--ssh-program", "ssh launcher",
    "--ssh-arg", "-o", "--ssh-arg", "argument ; with spaces"];
  assert.deepEqual(calls(), [
    {args: [...prefix, "list", "--json"], request: null},
    {args: [...prefix, "windows", ":3", "--json"], request: null},
  ]);
});

test("two independent clients keep their own named identities on the same target", async (t) => {
  const {calls, lifecycle, log} = setup(t);
  const options: CtreamOptions = {
    executable: [process.execPath, fixture, log, "ok"],
    agent: "display-agent", agent_id: "probe.A-1",
  };
  const first = new Ctream(options);
  options.agent_id = "probe_B";
  const second = new Ctream(options);
  options.agent_id = "changed.after.construction";
  assert.equal(first.agent_id, "probe.A-1");
  assert.equal(second.agent_id, "probe_B");
  const a = first.session(":3", "x11-1a");
  const b = await second.resolveWindow(":3", {title: "Document"});
  try {
    await Promise.all([a.screenshot(), b.capabilities()]);
    await Promise.all([a.click({x: 0, y: 0}), b.pressKeys("Tab")]);
  } finally {
    await Promise.all([a.close(), b.close()]);
  }
  const recorded = calls();
  assert.deepEqual(recorded[0].args, ["--agent", "display-agent", "windows", ":3", "--json"]);
  assert.equal(recorded.length, 5);
  for (const {args, request} of recorded.slice(1)) {
    const id = ["screenshot", "click"].includes(request.operation) ? "probe.A-1" : "probe_B";
    assert.deepEqual(args, ["--agent", "display-agent", "session", ":3", "--protocol", "1.0.3", "--agent-id", id, "--window", "x11-1a"]);
  }
  assert.equal(new Set(lifecycle().filter((event) => event.startsWith("start"))).size, 2);
});

test("agent identity is validated at construction", () => {
  for (const agent_id of [
    "", " leading", "trailing\n", "-cli", "_a", "a/b", "a b", "é", "a".repeat(65),
    "Otter#7c2a", "otter#7C2A", "o2#7c2a", "otter#123", "otter#12345", "otter#123g",
    "#1234", "otter#1234\n", "a".repeat(60) + "#1234",
  ]) {
    assert.throws(() => new Ctream({agent_id}), /agent_id/);
  }
  assert.throws(() => Reflect.construct(Ctream, [{agent_id: null}]), /agent_id/);
  for (const agent_id of ["7", "a".repeat(64), "unknownword#bad0", "a".repeat(59) + "#0000"]) {
    assert.equal(new Ctream({agent_id}).agent_id, agent_id);
  }
});

test("generated identity is readable and remains stable across sessions and retries", async (t) => {
  const words: string[] = JSON.parse(readFileSync(new URL("../dist/agent-words.json", import.meta.url), "utf8"));
  for (const [agent_id, protocol] of [[undefined, "1.0.4"], ["otter#7c2a", "1.0.4"], ["tester", "1.0.3"]] as const) {
    const {client, calls} = setup(t, "identity_unsupported", {agent_id});
    const id = client.agent_id;
    if (agent_id === undefined) {
      assert.match(id, /^[a-z]+#[a-f0-9]{4}$/);
      assert.ok(words.includes(id.split("#")[0]));
    } else {
      assert.equal(id, agent_id);
    }
    assert.equal(Reflect.set(client, "agent_id", "replacement"), false);
    for (let attempt = 0; attempt < 2; attempt++) {
      const session = client.session(":3");
      await assert.rejects(session.pressKeys("Tab"), /named sessions are unsupported/);
      await assert.rejects(session.screenshot(), /named sessions are unsupported/);
      await assert.rejects(session.close(), /named sessions are unsupported/);
    }
    const expected = {args: ["session", ":3", "--protocol", protocol, "--agent-id", id], request: null};
    assert.deepEqual(calls(), [expected, expected]);
  }
});

test("uses only the selected window's image dimensions and preserves explicit sizes", async (t) => {
  const {client, calls} = setup(t);
  const session = client.session(":3", "x11-1a");
  const point = {x: 0, y: 0};
  assert.deepEqual(await session.capabilities(), {capture: true, input: true, semantics: false});
  const frame = await session.screenshot();
  assert.equal(frame.mime_type, "image/png");
  assert.equal(Buffer.from(frame.data, "base64").subarray(1, 4).toString(), "PNG");
  await session.move(point);
  const match = {...point, text: "Continue"};
  await session.click(match);
  await session.drag(point, {x: 2, y: 3}, {button: "right", size: [9, 8]});
  await session.scroll(point, "down");
  const other = client.session(":3", "x11-2b");
  await assert.rejects(other.click(point), /screenshot/);
  await other.click(point, {size: [4, 5]});

  await Promise.all([session.close(), other.close()]);
  const recorded = calls();
  const args = ["session", ":3", "--protocol", "1.0.4", "--agent-id", client.agent_id, "--window"];
  assert.deepEqual(recorded.slice(0, -1).map((entry) => entry.args),
    Array(6).fill([...args, "x11-1a"]));
  assert.deepEqual(recorded.at(-1).args, [...args, "x11-2b"]);
  assert.deepEqual(recorded.map((entry) => entry.request), [
    {operation: "capabilities"}, {operation: "screenshot"},
    {operation: "move", point, size: [1, 1]},
    {operation: "click", point, button: "left", size: [1, 1]},
    {operation: "drag", from: point, to: {x: 2, y: 3}, button: "right", size: [9, 8]},
    {operation: "scroll", point, direction: "down", steps: 1, size: [1, 1]},
    {operation: "click", point, button: "left", size: [4, 5]},
  ]);
});

test("sends keyboard and semantic actions in JSON and returns structured controls", async (t) => {
  const {client, calls} = setup(t);
  const session = client.session("wayland-2", "window_1");
  const [element] = await session.elements();
  assert.equal(element.bounds?.x, 0.1);
  await session.invoke(element.path, 0, element.actions[0]);
  const text = 'a line\nquoted "text"';
  await session.setText(element.path, text);
  await session.pressKeys(["Control_L", "a"]);
  await session.typeText(text);
  await session.close();
  const recorded = calls();
  assert.deepEqual(recorded.map((entry) => entry.request), [
    {operation: "elements"}, {operation: "invoke", path: "0/1", index: 0, name: "press"},
    {operation: "set_text", path: "0/1", text},
    {operation: "press_keys", keys: ["Control_L", "a"]}, {operation: "type_text", text},
  ]);
});

test("resolves an exact window and translates shortcuts while keeping native keys", async (t) => {
  const {client, calls} = setup(t);
  await assert.rejects(client.resolveWindow(":3", {}), /nonempty/);
  await assert.rejects(client.resolveWindow(":3", {title: "Doc"}), /no window/);
  await assert.rejects(client.resolveWindow(":3", {app_id: "editor"}), /multiple windows/);
  const session = await client.resolveWindow(":3", {app_id: "editor", title: "New Document"});
  await session.pressKeys("cmd+a");
  await session.pressKeys(["Control_R", "enter"]);
  await session.pressKeys("cmd++");
  await session.pressKeys(["Shift_R", "plus"]);
  await assert.rejects(session.pressKeys("cmd+"), /nonempty/);
  await session.close();
  const chords = [["Super_L", "a"], ["Control_R", "Return"], ["Super_L", "Shift_L", "equal"], ["Shift_R", "equal"]];
  assert.deepEqual(calls().slice(-4), chords.map((keys) => ({
    args: ["session", ":3", "--protocol", "1.0.4", "--agent-id", client.agent_id, "--window", "x11-2b"],
    request: {operation: "press_keys", keys},
  })));
});

test("reports CLI failure, malformed output and excessive request size", async (t) => {
  const recoverable = setup(t, "failure").client.session(":0");
  await assert.rejects(recoverable.invoke("0", 0, "click"), /element is stale/);
  assert.equal((await recoverable.capabilities()).input, true);
  await recoverable.close();
  await assert.rejects(setup(t, "invalid_json").client.displays(), /invalid JSON/);
  const session = setup(t, "bad_shape").client.session(":0");
  await assert.rejects(session.typeText("水".repeat(22_000)), /64 KiB/);
  await assert.rejects(session.typeText("hello"), /expected null/);
  await assert.rejects(session.screenshot(), /expected null/);
  await assert.rejects(session.close(), /expected null/);
  await assert.rejects(new Ctream({executable: join(tmpdir(), "missing-ctream-executable")}).displays(), /ENOENT/);
});

test("accepts screenshots exceeding the default subprocess buffer", async (t) => {
  const session = setup(t, "large").client.session(":0");
  assert.ok((await session.screenshot()).data.length > 1024 * 1024);
  await session[Symbol.asyncDispose]();
});

test("opens lazily, orders concurrent requests, and drains admitted work on close", async (t) => {
  const {client, calls, lifecycle} = setup(t);
  const unused = client.session(":0");
  await unused.close();
  await assert.rejects(unused.capabilities(), /closed/);
  assert.deepEqual(lifecycle(), []);

  const session = client.session(":0");
  const shot = session.screenshot();
  const click = session.click({x: 0, y: 0});
  const close = session.close();
  assert.equal(close, session.close());
  await assert.rejects(session.typeText("late"), /closed/);
  await Promise.all([shot, click, close]);
  assert.deepEqual(calls().map(({request}) => request.operation), ["screenshot", "click"]);
  const [start] = lifecycle();
  assert.match(start, /^start \d+$/);
  assert.deepEqual(lifecycle(), [start, start.replace("start", "eof")]);
});

test("close propagates CLI cleanup errors at EOF", async (t) => {
  const session = setup(t, "cleanup_error").client.session(":0");
  await session.capabilities();
  await assert.rejects(session.close(), /remote input cleanup failed/);
  await assert.rejects(session.close(), /remote input cleanup failed/);
});

test("closing preserves an admitted request's configured deadline", async (t) => {
  const entry = new URL("../dist/session-transport.js", import.meta.url);
  const {SessionTransport} = await import(entry.href) as typeof import("../src/session-transport.js");
  const {log} = setup(t);
  const transport = new SessionTransport(
    () => spawn(process.execPath, [fixture, log, "slow", "session", ":0"]),
    2000, 75,
  );
  const request = transport.request({operation: "capabilities"}, (reply) => reply);
  const close = transport.close();
  assert.deepEqual(await request, {capture: true, input: true, semantics: false});
  await close;
});

test("a peer exiting after one reply cannot respawn for an already queued request", async (t) => {
  const {client, calls, lifecycle} = setup(t, "exit_after_reply");
  const session = client.session(":0");
  const results = await Promise.allSettled([session.capabilities(), session.typeText("queued")]);
  assert.equal(results[0].status, "fulfilled");
  assert.equal(results[1].status, "rejected");
  await assert.rejects(session.close());
  assert.deepEqual(calls().map(({request}) => request.operation), ["capabilities"]);
  assert.equal(lifecycle().filter((event) => event.startsWith("start")).length, 1);
});

test("reads split UTF-8 and rejects oversized or malformed session records", async (t) => {
  const session = setup(t, "chunked").client.session(":0");
  assert.equal((await session.elements())[0].name, "保存");
  await session.close();
  for (const [mode, error] of [["overflow", /byte limit/], ["invalid_json", /invalid ctream session reply/]] as const) {
    const bad = setup(t, mode).client.session(":0");
    await assert.rejects(bad.capabilities(), error);
    await assert.rejects(bad.close(), error);
  }
});

test("a dead or stalled session rejects queued actions without replay", async (t) => {
  for (const mode of ["exit", "hang"]) {
    const {client, calls, lifecycle} = setup(t, mode, {timeout_ms: mode === "hang" ? 800 : 0});
    const session = client.session(":0");
    const results = await Promise.allSettled([session.typeText("one"), session.typeText("two")]);
    const diagnostic = mode === "hang" ? /timed out/ : /window disappeared/;
    for (const result of results) {
      assert.equal(result.status, "rejected");
      assert.match(result.reason.message, diagnostic);
    }
    await assert.rejects(session.typeText("three"), diagnostic);
    await assert.rejects(session.close(), diagnostic);
    assert.deepEqual(calls().map(({request}) => request.text), ["one"]);
    assert.equal(lifecycle().filter((event) => event.startsWith("start")).length, 1);
  }
});

test("an idle session permits natural caller exit without an explicit close", async (t) => {
  const {log, lifecycle} = setup(t);
  const entry = new URL("../dist/index.js", import.meta.url).href;
  const program = `
    import {Ctream} from ${JSON.stringify(entry)};
    const client = new Ctream({executable: [process.execPath, ...process.argv.slice(1), "ok"]});
    const session = client.session(":0");
    await session.capabilities();
    await session.screenshot();
    console.log("finished");
  `;
  const {stdout} = await promisify(execFile)(process.execPath, ["--input-type=module", "-e", program, "--", fixture, log], {timeout: 5000});
  assert.match(stdout, /finished/);
  const deadline = Date.now() + 2000;
  while (lifecycle().length < 2 && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.match(lifecycle()[1], /^eof \d+$/);
});
