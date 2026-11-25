import assert from "node:assert/strict";
import { appendFileSync, readFileSync } from "node:fs";

// An SSH-compatible launcher. Check the actual request produced by the Rust
// client, including contract and window identity, without opening a connection.
const [log, mode, host, command] = process.argv.slice(2);
assert.equal(mode, "-T");
assert.equal(host, "fixture");
switch (command) {
  case "exec 'ctream-agent' '1.0.11' 'discover'":
    console.log(JSON.stringify([{ display: { x11: 2 }, selectable: true, class: null }]));
    break;
  case "exec 'ctream-agent' '1.0.3' 'windows' ':2'":
    console.log(JSON.stringify([{ id: "x11-7", title: "Editor", app_id: "editor" }]));
    break;
  case "exec 'ctream-agent' '1.0.3' 'capture-window' ':2' 'x11-7'":
    process.stdout.write("P6\n2 1\n255\nabcdef");
    break;
  case "exec 'ctream-agent' '1.1.12' 'input-window' ':2' 'x11-7'": {
    process.stdout.write("ready\n");
    const events = readFileSync(0, "utf8");
    appendFileSync(log, events);
    if (events.includes("key Tab")) {
      console.error("window disappeared");
      process.exitCode = 1;
    }
    break;
  }
  default:
    throw new Error(`unexpected ctream-agent invocation ${command}`);
}
