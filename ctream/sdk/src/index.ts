import { cli, type CtreamOptions } from "./command.js";
import {
  displaySession, expectShape, isCapabilities, isElement, isScreenshot, isWindow,
  type Button, type Capabilities, type Direction, type DisplaySession, type Element,
  type Point, type Screenshot, type Size, type Window,
} from "./model.js";

export type { CtreamOptions } from "./command.js";
export { findText, type TextMatch, type OcrOptions } from "./ocr.js";
export type {
  Button, Capabilities, Direction, Display, DisplaySession, Element, Point, Screenshot, Size, Window,
} from "./model.js";

export class Ctream {
  readonly #run: ReturnType<typeof cli>;

  constructor(options: CtreamOptions = {}) {
    this.#run = cli(options);
  }

  async displays(): Promise<DisplaySession[]> {
    const reply = await this.#run(["list", "--json"]);
    expectShape(Array.isArray(reply), "display list");
    return reply.map(displaySession);
  }

  async windows(target: string): Promise<Window[]> {
    const reply = await this.#run(["windows", target, "--json"]);
    expectShape(Array.isArray(reply) && reply.every(isWindow), "window list");
    return reply;
  }

  /** Select exactly one window by its discovered app_id, title, or both. */
  async resolveWindow(target: string, {app_id, title}: {app_id?: string; title?: string}): Promise<CtreamSession> {
    const fields = [app_id, title].filter((value) => value !== undefined);
    if (fields.length === 0 || fields.some((value) => !value.trim())) {
      throw new Error("provide a nonempty app_id or title to resolve a window");
    }
    const matches = (await this.windows(target)).filter((window) =>
      (app_id === undefined || app_id === window.app_id) && (title === undefined || title === window.title));
    if (!matches.length) throw new Error("no window matches the selector");
    if (matches.length > 1) throw new Error("multiple windows match the selector");
    return this.session(target, matches[0].id);
  }

  /** Bind observation and input to a single display or a discovered window ID. */
  session(target: string, window_id?: string): CtreamSession {
    const args = ["do", target, "--protocol", "1.0.1"];
    if (window_id !== undefined) args.push("--window", window_id);
    return new CtreamSession((request) => this.#run(args, request));
  }
}

type Request = {operation: string} & Record<string, unknown>;
type PointerOptions = {button?: Button; size?: Size};

// OCR matches and other structurally compatible points may carry additional data.
function coordinates({x, y}: Point): Point {
  return {x, y};
}

const keyAliases = new Map(Object.entries({
  cmd: "Super_L", command: "Super_L", super: "Super_L", meta: "Super_L",
  ctrl: "Control_L", control: "Control_L", alt: "Alt_L", option: "Alt_L", shift: "Shift_L",
  enter: "Return", return: "Return", esc: "Escape", escape: "Escape", backspace: "BackSpace",
  tab: "Tab", space: "space", plus: "plus", "+": "plus",
}));

function chordKeys(keys: string | readonly string[]): string[] {
  let tokens: readonly string[] = typeof keys === "string" ? keys.split("+") : keys;
  if (typeof keys === "string" && keys === "+") tokens = ["plus"];
  if (typeof keys === "string" && keys.endsWith("++")) tokens = [...keys.slice(0, -2).split("+"), "plus"];
  const names = tokens.map((token) => {
    const name = token.trim();
    if (!name) throw new Error("expected a nonempty key name in chord");
    return keyAliases.get(name.toLowerCase()) ?? name;
  });
  if (!names.length) throw new Error("expected a nonempty chord");
  const plus = names.indexOf("plus");
  if (plus !== -1) {
    const shift = names.findIndex((key) => key === "Shift_L" || key === "Shift_R");
    // On the US physical layout a plus is the equal key with Shift already down.
    if (shift === -1) names.splice(plus, 0, "Shift_L");
    if (shift > plus) {
      const [modifier] = names.splice(shift, 1);
      names.splice(plus, 0, modifier);
    }
    return names.map((key) => key === "plus" ? "equal" : key);
  }
  return names;
}

class CtreamSession {
  readonly #request: (request: Request) => Promise<unknown>;
  #last_size?: Size;

  constructor(request: (request: Request) => Promise<unknown>) {
    this.#request = request;
  }

  async capabilities(): Promise<Capabilities> {
    const reply = await this.#request({operation: "capabilities"});
    expectShape(isCapabilities(reply), "session capabilities");
    return reply;
  }

  async screenshot(): Promise<Screenshot> {
    const reply = await this.#request({operation: "screenshot"});
    expectShape(isScreenshot(reply), "PNG screenshot");
    this.#last_size = [reply.width, reply.height];
    return reply;
  }

  #size(size?: Size): Size {
    const dimensions = size ?? this.#last_size;
    if (!dimensions) {
      throw new Error("call screenshot() on this session first or supply its image size");
    }
    return dimensions;
  }

  async #act(request: Request): Promise<void> {
    const reply = await this.#request(request);
    expectShape(reply === null, "null after applying the action");
  }

  async move(point: Point, size?: Size): Promise<void> {
    await this.#act({operation: "move", point: coordinates(point), size: this.#size(size)});
  }

  async click(point: Point, {button = "left", size}: PointerOptions = {}): Promise<void> {
    await this.#act({operation: "click", point: coordinates(point), button, size: this.#size(size)});
  }

  async drag(from: Point, to: Point, {button = "left", size}: PointerOptions = {}): Promise<void> {
    await this.#act({operation: "drag", from: coordinates(from), to: coordinates(to), button, size: this.#size(size)});
  }

  async scroll(point: Point, direction: Direction, {steps = 1, size}: {steps?: number; size?: Size} = {}): Promise<void> {
    await this.#act({operation: "scroll", point: coordinates(point), direction, steps, size: this.#size(size)});
  }

  /** A simultaneous chord such as "cmd+a" or "cmd++" (US physical keys). */
  async pressKeys(keys: string | readonly string[]): Promise<void> {
    await this.#act({operation: "press_keys", keys: chordKeys(keys)});
  }

  async typeText(text: string): Promise<void> {
    await this.#act({operation: "type_text", text});
  }

  async elements(): Promise<Element[]> {
    const reply = await this.#request({operation: "elements"});
    expectShape(Array.isArray(reply) && reply.every(isElement), "element list");
    return reply;
  }

  async invoke(path: string, index: number, name: string): Promise<void> {
    await this.#act({operation: "invoke", path, index, name});
  }

  async setText(path: string, text: string): Promise<void> {
    await this.#act({operation: "set_text", path, text});
  }
}

export type { CtreamSession };
