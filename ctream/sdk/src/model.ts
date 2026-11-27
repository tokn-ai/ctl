export interface Point {
  x: number;
  y: number;
}
export type Size = readonly [width: number, height: number];
export type Button = "left" | "middle" | "right";
export type Direction = "up" | "down" | "left" | "right";

export type Display = { x11: number }
  | { wayland: { uid: number; socket: string } }
  | { windows: { number: number; name: string; bounds: [number, number, number, number] } }
  | "macos_desktop";

export interface DisplaySession {
  display: Display;
  class: string | null;
  selectable: boolean;
  /** Selection argument for this session, or null when inaccessible to this account. */
  target: string | null;
}

export interface Window {
  id: string;
  title: string;
  app_id: string;
}

export interface Capabilities {
  capture: boolean;
  input: boolean;
  semantics: boolean;
}

export interface Screenshot {
  width: number;
  height: number;
  mime_type: "image/png";
  /** Base64-encoded PNG. Use Buffer.from(data, "base64") for bytes. */
  data: string;
}

export interface Element {
  path: string;
  name: string;
  role: string;
  actions: string[];
  editable: boolean;
  /** Fractions of the selected window's image dimensions. */
  bounds?: {x: number; y: number; width: number; height: number};
}

export function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

export function validAgentId(value: unknown): value is string {
  return typeof value === "string" && /^(?:[A-Za-z0-9][A-Za-z0-9._-]{0,63}|[a-z]{1,59}#[0-9a-f]{4})$/.test(value);
}

export function expectShape(condition: unknown, expected: string): asserts condition {
  if (!condition) throw new Error(`invalid ctream reply: expected ${expected}`);
}

function unsigned(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0 && (value as number) <= 0xffffffff;
}

export function displaySession(value: unknown): DisplaySession {
  expectShape(record(value) && typeof value.selectable === "boolean" &&
    (value.class == null || typeof value.class === "string"), "display session");
  const display = value.display;
  let target: string;
  if (display === "macos_desktop") {
    target = "macos";
  } else if (record(display) && unsigned(display.x11)) {
    target = `:${display.x11}`;
  } else if (record(display) && record(display.wayland) && unsigned(display.wayland.uid) &&
      typeof display.wayland.socket === "string" && /^wayland-\d+$/.test(display.wayland.socket)) {
    target = display.wayland.socket;
  } else if (record(display) && record(display.windows) && unsigned(display.windows.number) &&
      display.windows.number > 0 && typeof display.windows.name === "string" &&
      Array.isArray(display.windows.bounds) && display.windows.bounds.length === 4 &&
      display.windows.bounds.every(Number.isInteger)) {
    target = `windows-${display.windows.number}`;
  } else {
    throw new Error("invalid ctream reply: unrecognized display");
  }
  return {
    display: display as Display,
    class: value.class ?? null,
    selectable: value.selectable,
    target: value.selectable ? target : null,
  };
}

export function isWindow(value: unknown): value is Window {
  return record(value) && typeof value.id === "string" && typeof value.title === "string" &&
    typeof value.app_id === "string";
}

export function isCapabilities(value: unknown): value is Capabilities {
  return record(value) && typeof value.capture === "boolean" && typeof value.input === "boolean" &&
    typeof value.semantics === "boolean";
}

export function isScreenshot(value: unknown): value is Screenshot {
  return record(value) && value.mime_type === "image/png" && typeof value.data === "string" &&
    value.data.length > 0 && unsigned(value.width) && value.width > 0 &&
    unsigned(value.height) && value.height > 0;
}

export function isElement(value: unknown): value is Element {
  return record(value) && typeof value.path === "string" && typeof value.name === "string" &&
    typeof value.role === "string" && typeof value.editable === "boolean" &&
    Array.isArray(value.actions) && value.actions.every((action) => typeof action === "string") &&
    (value.bounds === undefined || (record(value.bounds) && (
      [value.bounds.x, value.bounds.y, value.bounds.width, value.bounds.height].every(Number.isFinite)
    )));
}
