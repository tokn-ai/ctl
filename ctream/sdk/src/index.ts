import type { Request, SessionTransport } from "./session-transport.js";
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
  readonly #cli: ReturnType<typeof cli>;

  constructor(options: CtreamOptions = {}) {
    this.#cli = cli(options);
  }

  /** Stable identity used by every session opened by this client. */
  get agent_id(): string {
    return this.#cli.agent_id;
  }

  async displays(): Promise<DisplaySession[]> {
    const reply = await this.#cli.query(["list", "--json"]);
    expectShape(Array.isArray(reply), "display list");
    return reply.map(displaySession);
  }

  async windows(target: string): Promise<Window[]> {
    const reply = await this.#cli.query(["windows", target, "--json"]);
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
    return new CtreamSession(this.#cli.session(target, window_id));
  }
}

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
  readonly #transport: SessionTransport;
  #last_size?: Size;

  constructor(transport: SessionTransport) {
    this.#transport = transport;
  }

  /** Drain admitted requests and release this session's capture and input resources. */
  close(): Promise<void> {
    return this.#transport.close();
  }

  [Symbol.asyncDispose](): Promise<void> {
    return this.close();
  }

  async capabilities(): Promise<Capabilities> {
    return this.#transport.request({operation: "capabilities"}, (reply) => {
      expectShape(isCapabilities(reply), "session capabilities");
      return reply;
    });
  }

  async screenshot(): Promise<Screenshot> {
    return this.#transport.request({operation: "screenshot"}, (reply) => {
      expectShape(isScreenshot(reply), "PNG screenshot");
      this.#last_size = [reply.width, reply.height];
      return reply;
    });
  }

  #act(request: Request | (() => Request)): Promise<void> {
    return this.#transport.request(request, (reply) => {
      expectShape(reply === null, "null after applying the action");
    });
  }

  #pointer(action: Request, size?: Size): Promise<void> {
    const explicit: Size | undefined = size && [size[0], size[1]];
    return this.#act(() => {
      const dimensions = explicit ?? this.#last_size;
      if (!dimensions) throw new Error("call screenshot() on this session first or supply its image size");
      return {...action, size: dimensions};
    });
  }

  async move(point: Point, size?: Size): Promise<void> {
    await this.#pointer({operation: "move", point: coordinates(point)}, size);
  }

  async click(point: Point, {button = "left", size}: PointerOptions = {}): Promise<void> {
    await this.#pointer({operation: "click", point: coordinates(point), button}, size);
  }

  async drag(from: Point, to: Point, {button = "left", size}: PointerOptions = {}): Promise<void> {
    await this.#pointer({operation: "drag", from: coordinates(from), to: coordinates(to), button}, size);
  }

  async scroll(point: Point, direction: Direction, {steps = 1, size}: {steps?: number; size?: Size} = {}): Promise<void> {
    await this.#pointer({operation: "scroll", point: coordinates(point), direction, steps}, size);
  }

  /** A simultaneous chord such as "cmd+a" or "cmd++" (US physical keys). */
  async pressKeys(keys: string | readonly string[]): Promise<void> {
    await this.#act({operation: "press_keys", keys: chordKeys(keys)});
  }

  async typeText(text: string): Promise<void> {
    await this.#act({operation: "type_text", text});
  }

  async elements(): Promise<Element[]> {
    return this.#transport.request({operation: "elements"}, (reply) => {
      expectShape(Array.isArray(reply) && reply.every(isElement), "element list");
      return reply;
    });
  }

  async invoke(path: string, index: number, name: string): Promise<void> {
    await this.#act({operation: "invoke", path, index, name});
  }

  async setText(path: string, text: string): Promise<void> {
    await this.#act({operation: "set_text", path, text});
  }
}

export type { CtreamSession };
