import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
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
  return {client, calls};
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

  const recorded = calls();
  const args = ["do", ":3", "--protocol", "1.0.1", "--window"];
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
  const chords = [["Super_L", "a"], ["Control_R", "Return"], ["Super_L", "Shift_L", "equal"], ["Shift_R", "equal"]];
  assert.deepEqual(calls().slice(-4), chords.map((keys) => ({
    args: ["do", ":3", "--protocol", "1.0.1", "--window", "x11-2b"],
    request: {operation: "press_keys", keys},
  })));
});

test("reports CLI failure, malformed output and excessive request size", async (t) => {
  await assert.rejects(setup(t, "failure").client.session(":0").invoke("0", 0, "click"), /element is stale/);
  await assert.rejects(setup(t, "invalid_json").client.displays(), /invalid JSON/);
  const session = setup(t, "bad_shape").client.session(":0");
  await assert.rejects(session.typeText("hello"), /expected null/);
  await assert.rejects(session.screenshot(), /expected PNG/);
  await assert.rejects(session.typeText("水".repeat(22_000)), /64 KiB/);
  await assert.rejects(new Ctream({executable: join(tmpdir(), "missing-ctream-executable")}).displays(), /ENOENT/);
});

test("accepts screenshots exceeding the default subprocess buffer", async (t) => {
  const frame = await setup(t, "large").client.session(":0").screenshot();
  assert.ok(frame.data.length > 1024 * 1024);
});
