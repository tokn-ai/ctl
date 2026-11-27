import { randomUUID } from "node:crypto";
import { appendFileSync, existsSync } from "node:fs";
import { createInterface } from "node:readline";

const [log, mode, ...args] = process.argv.slice(2);
const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVQI12NgYGAAAAAEAAEnNCcKAAAAAElFTkSuQmCC";
const element = {
  path: "0/1", role: "button", name: "Save", editable: false, actions: ["press"],
  bounds: {x: 0.1, y: 0.2, width: 0.3, height: 0.4},
};

function record(request: unknown) {
  appendFileSync(log, JSON.stringify({args, request}) + "\n");
}

let currentTurn: string | undefined;
let turnCount = 0;
function handleTurn(request: {operation: string; turn_id?: string}): boolean {
  const busy = {ok: false, error: "desktop is busy", code: "busy", owner_id: "session-owner"};
  if (request.operation === "acquire_turn") {
    if (currentTurn) console.log(JSON.stringify(busy));
    else {
      currentTurn = `turn-${++turnCount}`;
      console.log(JSON.stringify({ok: true, result: {turn_id: currentTurn}}));
    }
    return true;
  }
  if (request.turn_id && (currentTurn !== request.turn_id || mode === "expired_turn")) {
    console.log(JSON.stringify({ok: false, error: "expired turn", code: "stale_turn"}));
    return true;
  }
  if (request.operation === "release_turn") currentTurn = undefined;
  if (currentTurn && !request.turn_id && !["capabilities", "screenshot", "elements"].includes(request.operation)) {
    console.log(JSON.stringify({ok: false, code: "stale_turn", error: "the owner must supply its turn ID", action_id: randomUUID()}));
    return true;
  }
  return false;
}

if (args.includes("session")) {
  const lifecycle = log + ".lifecycle";
  appendFileSync(lifecycle, `start ${process.pid}\n`);
  if (mode === "identity_unsupported" && args.includes("--agent-id")) {
    record(null);
    console.error("named sessions are unsupported by this agent");
    process.exit(1);
  }
  for await (const line of createInterface({input: process.stdin, crlfDelay: Infinity})) {
    const request = JSON.parse(line);
    record(request);
    if (args.includes("1.0.7") && handleTurn(request)) continue;
    if (mode === "hang") continue;
    if (mode === "slow") await new Promise((resolve) => setTimeout(resolve, 150));
    if (mode === "exit") {
      console.error("window disappeared during input");
      process.exit(2);
    }
    if (mode === "invalid_json") {
      process.stdout.write("malformed\n");
      continue;
    }
    if (mode === "overflow") {
      process.stdout.write("a".repeat(2 * 1024 * 1024 + 1));
      continue;
    }
    if (mode === "failure" && request.operation === "invoke") {
      process.stdout.write(JSON.stringify({ok: false, error: "the selected element is stale"}) + "\n");
      continue;
    }
    let result;
    switch (request.operation) {
      case "capabilities": result = {capture: true, input: true, semantics: false}; break;
      case "screenshot": result = {width: 1, height: 1, mime_type: "image/png", data: mode === "large" ? png.repeat(12_000) : png}; break;
      case "elements": result = [{...element, name: mode === "chunked" ? "保存" : element.name}]; break;
      default: result = null;
    }
    const action_id = args.includes("1.0.7") && result === null && request.operation !== "release_turn"
      ? randomUUID() : undefined;
    const message = JSON.stringify({ok: true, action_id, result: mode === "bad_shape" ? {} : result}) + "\n";
    if (mode === "exit_after_reply") {
      await new Promise<void>((resolve) => process.stdout.write(message, () => resolve()));
      process.exit(0);
    }
    if (mode === "chunked") {
      const bytes = Buffer.from(message);
      const at = bytes.indexOf(Buffer.from("保")) + 1;
      process.stdout.write(bytes.subarray(0, at));
      await new Promise((resolve) => setTimeout(resolve, 10));
      process.stdout.write(bytes.subarray(at));
    } else {
      process.stdout.write(message);
    }
  }
  if (existsSync(lifecycle)) appendFileSync(lifecycle, `eof ${process.pid}\n`);
  if (mode === "cleanup_error") {
    console.error("remote input cleanup failed");
    process.exitCode = 3;
  }
} else {
  record(null);
  if (mode === "invalid_json") {
    process.stdout.write("line of text");
  } else if (args.includes("list")) {
    process.stdout.write(JSON.stringify([
      {display: {x11: 3}, class: null, selectable: true},
      {display: {wayland: {uid: 42, socket: "wayland-2"}}, class: "user", selectable: true},
      {display: {wayland: {uid: 77, socket: "wayland-2"}}, class: null, selectable: false},
      {display: {windows: {number: 1, name: "screen", bounds: [-1280, 0, 0, 720]}}, class: "interactive", selectable: true},
      {display: "macos_desktop", class: "interactive", selectable: true},
    ]));
  } else {
    process.stdout.write(JSON.stringify([
      {id: "x11-1a", title: "Document", app_id: "editor"},
      {id: "x11-2b", title: "New Document", app_id: "editor"},
    ]));
  }
}
