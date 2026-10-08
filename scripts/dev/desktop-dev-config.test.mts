import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { desktopDevArguments } from "./desktop-dev-config.mts";

const config = JSON.parse(await readFile(
  new URL("../../apps/desktop/src-tauri/tauri.conf.json", import.meta.url), "utf8",
));

test("the selected URL and its websocket origin replace only development CSP sources", () => {
  const before = structuredClone(config);
  const args = desktopDevArguments([], { url: "http://127.0.0.1:51943/", config, platform: "darwin" });
  const override = JSON.parse(args[1]!);
  assert.equal(override.build.devUrl, "http://127.0.0.1:51943/");
  const csp = override.app.security.devCsp;
  assert.match(csp, /http:\/\/127\.0\.0\.1:51943/);
  assert.match(csp, /ws:\/\/127\.0\.0\.1:51943/);
  assert.doesNotMatch(csp, /1430|1431|\*/);
  assert.match(csp, /ipc: http:\/\/ipc\.localhost/);
  assert.equal(override.app.security.csp, undefined);
  assert.deepEqual(config, before);
});

test("the runtime config follows user configs but precedes runner and application arguments", () => {
  const args = desktopDevArguments(["--release", "--config", "custom.json", "--", "--features", "example", "--", "--help"], {
    url: "http://127.0.0.1:51943/", config, platform: "linux",
  });
  assert.deepEqual(args.slice(0, 4), ["--release", "--config", "custom.json", "--config"]);
  assert.deepEqual(args.slice(5), ["--", "--features", "example", "--", "--help"]);
  assert.equal(JSON.parse(args[4]!).build.devUrl, "http://127.0.0.1:51943/");
});

test("native preflight retains Windows daemon builds and never starts a second frontend", () => {
  for (const platform of ["darwin", "linux", "win32"] as const) {
    const args = desktopDevArguments([], { url: "http://127.0.0.1:51943/", config, platform });
    const command = JSON.parse(args[1]!).build.beforeDevCommand;
    assert.equal(command.wait, true);
    assert.ok(command.script.endsWith("pnpm --workspace-root bundles:check"));
    assert.equal(command.script.includes("daemons:build"), platform === "win32");
    assert.doesNotMatch(command.script, /ctmux-app|\bdev\b/);
  }
});

test("IPv6 HTTPS URLs use matching secure websocket sources without replacing similar text", () => {
  const input = structuredClone(config);
  input.app.security.devCsp += "; report-uri http://localhost:14300/report";
  const args = desktopDevArguments([], { url: "https://[::1]:51943/", config: input, platform: "linux" });
  const csp = JSON.parse(args[1]!).app.security.devCsp;
  assert.match(csp, /https:\/\/\[::1\]:51943/);
  assert.match(csp, /wss:\/\/\[::1\]:51943/);
  assert.match(csp, /http:\/\/localhost:14300\/report/);
});
