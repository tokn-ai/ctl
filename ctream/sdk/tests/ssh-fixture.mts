import assert from "node:assert/strict";
import { appendFileSync, existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { createInterface } from "node:readline";

// Exercise the real CLI's process transport against an instrumented display host.
const [dir, profile, mode, host, command] = process.argv.slice(2);
assert.equal(mode, "-T");
assert.equal(host, "fixture");
const changed = join(dir, "changed");
const log = (event: string) => appendFileSync(join(dir, "lifecycle"), `${process.ppid} ${event}\n`);
const events = join(dir, "events");
const tagged = /^exec 'ctream-agent' '1\.1\.16' 'attributed-session' ':2' '([a-z]+#[0-9a-f]{4})' 'x11-7'$/.exec(command);

switch (command) {
  case tagged?.[0]: {
    assert.ok(tagged);
    log("attributed open");
    if (profile === "legacy") {
      console.error("unsupported attributed-session version");
      process.exitCode = 1;
      break;
    }
    for await (const line of createInterface({input: process.stdin})) {
      const request = JSON.parse(line);
      if (request.operation === "capabilities") {
        console.log(JSON.stringify({ok: true, result: {capture: true, input: true, semantics: false}}));
      } else {
        assert.equal(request.operation, "press_keys");
        appendFileSync(events, `${tagged[1]} ${request.keys.join(" ")}\n`);
        console.log(JSON.stringify({ok: true, result: null}));
      }
    }
    log("attributed close");
    break;
  }
  case "exec 'ctream-agent' '1.0.11' 'discover'":
    console.log(JSON.stringify([{ display: { x11: 2 }, selectable: true, class: null }]));
    break;
  case "exec 'ctream-agent' '1.0.3' 'windows' ':2'":
    console.log(JSON.stringify([{ id: "x11-7", title: "Editor", app_id: "editor" }]));
    break;
  case "exec 'ctream-agent' '1.0.3' 'capture-window' ':2' 'x11-7'": {
    log("capture open");
    process.stdout.write("P6\n2 1\n255\nabcdef");
    let resized = false;
    const timer = setInterval(() => {
      if (existsSync(changed) && !resized) {
        resized = true;
        process.stdout.write("P6\n1 1\n255\nxyz");
      }
    }, 10);
    process.stdin.on("end", () => {
      clearInterval(timer);
      log("capture close");
    }).resume();
    break;
  }
  case "exec 'ctream-agent' '1.1.14' 'input-capabilities'":
    log("probe");
    if (profile === "legacy") {
      console.error("usage: ctream-agent VERSION OPERATION [DISPLAY [WINDOW]]");
      process.exitCode = 1;
    } else if (profile === "denied") {
      console.error("agent access denied");
      process.exitCode = 1;
    } else {
      console.log(JSON.stringify({ sync: true }));
    }
    break;
  case "exec 'ctream-agent' '1.1.14' 'input-sync' ':2' 'x11-7'": {
    assert.equal(profile, "current");
    log("input open");
    process.stdout.write("ready\n");
    const lines = createInterface({ input: process.stdin });
    for await (const line of lines) {
      if (line === "sync") {
        process.stdout.write("ok\n");
      } else {
        appendFileSync(events, line + "\n");
        if (line === "button 1 0") writeFileSync(changed, "");
        if (line === "key Tab 1") {
          console.error("window disappeared");
          process.exit(1);
        }
      }
    }
    log("input close");
    break;
  }
  case "exec 'ctream-agent' '1.1.12' 'input-window' ':2' 'x11-7'": {
    assert.equal(profile, "legacy");
    log("legacy open");
    process.stdout.write("ready\n");
    const batch = readFileSync(0, "utf8");
    appendFileSync(events, batch);
    if (batch.includes("button 1 0")) writeFileSync(changed, "");
    log("legacy close");
    if (batch.includes("key Tab")) {
      console.error("window disappeared");
      process.exitCode = 1;
    }
    break;
  }
  default:
    throw new Error(`unexpected ctream-agent invocation ${command}`);
}
