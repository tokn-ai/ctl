import { appendFileSync } from "node:fs";
import { once } from "node:events";

const [log, mode, ...args] = process.argv.slice(2);
const self_id = String(process.pid);
const self = {participant_id: self_id, agent_id: args[args.indexOf("--agent-id") + 1]};
const peer = {participant_id: "other-session", agent_id: "writer"};
const snapshot = {type: "snapshot", self_id, participants: [self]};
const joined = {type: "joined", participant: peer};
const action = (value: unknown) => ({type: "action", participant_id: peer.participant_id, action: value});
const line = (value: unknown) => JSON.stringify(value) + "\n";
const write = (value: string) => new Promise<void>((resolve) => process.stdout.write(value, () => resolve()));

appendFileSync(log, line({started: self_id, args}));
if (mode === "reject") {
  console.error("watch is unsupported on this display host");
  process.exit(1);
}

// Keep the child's stdin open until the observer leaves. Use stdout backpressure
// for the flood case, as a real host would when its pipe fills.
const eof = once(process.stdin, "end").then(() => appendFileSync(log, line({left: self_id})));
process.stdin.resume();
if (mode === "turns") {
  const held = {type: "turn", holder: peer, expires_at_ms: 12345};
  const free = {type: "turn", holder: null, expires_at_ms: null};
  const publicAction = {...action({operation: "press_keys"}), action_id: "12345678-1234-1234-1234-123456789abc"};
  await write([snapshot, held, joined, publicAction, free].map(line).join(""));
  process.exit(0);
} else if (mode === "batch") {
  const batch = [
    snapshot, joined, action({operation: "click", point: [0, 1]}),
    action({operation: "drag", from: [0.1, 0.3], to: [0.9, 0.2]}),
    action({operation: "type_text"}), {type: "left", participant_id: peer.participant_id},
  ].map(line).join("");
  process.stdout.write(batch.slice(0, 6));
  await new Promise((resolve) => setTimeout(resolve, 10));
  await write(batch.slice(6));
  process.exit(0);
} else if (mode === "overflow") {
  process.stdout.write(" ".repeat(64 * 1024 + 1));
} else if (mode === "limit") {
  await write(JSON.stringify(snapshot).padEnd(64 * 1024) + "\n");
  process.exit(0);
} else if (mode === "truncated") {
  await write(line(snapshot).slice(0, -1));
  process.exit(0);
} else if (mode === "invalid_json") {
  process.stdout.write(Buffer.from([0xff, 10]));
} else if (mode === "wrong_first") {
  process.stdout.write(line(joined));
} else if (mode === "duplicate") {
  process.stdout.write(line({...snapshot, participants: [self, self]}));
} else if (mode === "missing_first") {
  process.exit(0);
} else if (mode === "wrong_point") {
  process.stdout.write(line(snapshot) + line(action({operation: "move", point: [-1, 0]})));
} else {
  await write(line(snapshot));
  if (mode === "exit") {
    console.error("display host disconnected");
    process.exit(4);
  } else if (mode === "flood") {
    await new Promise((resolve) => setTimeout(resolve, 20));
    const block = line(joined).repeat(256);
    for (let index = 0; index < 512; index++) {
      if (!process.stdout.write(block)) await once(process.stdout, "drain");
    }
    appendFileSync(log, line({sent_all: true}));
    await eof;
  } else {
    await eof;
  }
}
