import { appendFileSync, readFileSync } from "node:fs";

const [log, mode, ...args] = process.argv.slice(2);
const stdin = readFileSync(0, "utf8");
const request = stdin ? JSON.parse(stdin) : null;
appendFileSync(log, JSON.stringify({args, request}) + "\n");
if (mode === "failure") {
  console.error("the selected element is stale");
  process.exit(1);
}
if (mode === "invalid_json") {
  process.stdout.write("line of text");
  process.exit(0);
}

const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVQI12NgYGAAAAAEAAEnNCcKAAAAAElFTkSuQmCC";
const element = {
  path: "0/1", role: "button", name: "Save", editable: false, actions: ["press"],
  bounds: {x: 0.1, y: 0.2, width: 0.3, height: 0.4},
};
let result;
if (mode === "bad_shape") {
  result = {};
} else if (args.includes("list")) {
  result = [
    {display: {x11: 3}, class: null, selectable: true},
    {display: {wayland: {uid: 42, socket: "wayland-2"}}, class: "user", selectable: true},
    {display: {wayland: {uid: 77, socket: "wayland-2"}}, class: null, selectable: false},
    {display: {windows: {number: 1, name: "screen", bounds: [-1280, 0, 0, 720]}}, class: "interactive", selectable: true},
    {display: "macos_desktop", class: "interactive", selectable: true},
  ];
} else if (!request) {
  result = [
    {id: "x11-1a", title: "Document", app_id: "editor"},
    {id: "x11-2b", title: "New Document", app_id: "editor"},
  ];
} else {
  switch (request.operation) {
    case "capabilities": result = {capture: true, input: true, semantics: false}; break;
    case "screenshot": result = {width: 1, height: 1, mime_type: "image/png", data: mode === "large" ? png.repeat(12_000) : png}; break;
    case "elements": result = [element]; break;
    default: result = null;
  }
}
process.stdout.write(JSON.stringify(result));
