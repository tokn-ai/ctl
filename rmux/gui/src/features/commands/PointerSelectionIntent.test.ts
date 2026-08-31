import { describe, expect, it } from "vitest";
import { PointerSelectionIntent } from "./PointerSelectionIntent";

describe("PointerSelectionIntent", () => {
  it("selects only when physical movement crosses a command boundary", () => {
    const intent = new PointerSelectionIntent();

    expect(intent.move({ clientX: 10, clientY: 10 }, "first")).toBe("first");
    expect(intent.move({ clientX: 12, clientY: 12 }, "first")).toBeNull();
    expect(intent.move({ clientX: 12, clientY: 13 }, "second")).toBe(
      "second",
    );
  });

  it("does not select content that scrolls beneath a stationary pointer", () => {
    const intent = new PointerSelectionIntent(6);
    intent.move({ clientX: 20, clientY: 20 }, "first");

    intent.scrolled("fourth");

    expect(intent.move({ clientX: 21, clientY: 21 }, "fifth")).toBeNull();
    expect(intent.move({ clientX: 22, clientY: 22 }, "fifth")).toBeNull();
  });

  it("reactivates after deliberate post-scroll movement", () => {
    const intent = new PointerSelectionIntent(6);
    intent.move({ clientX: 20, clientY: 20 }, "first");
    intent.scrolled("fourth");

    expect(intent.move({ clientX: 22, clientY: 22 }, "fifth")).toBeNull();
    expect(intent.move({ clientX: 27, clientY: 22 }, "fifth")).toBe("fifth");
    expect(intent.move({ clientX: 28, clientY: 22 }, "sixth")).toBe("sixth");
  });

  it("forgets stale pointer state after leaving the result list", () => {
    const intent = new PointerSelectionIntent();
    intent.move({ clientX: 10, clientY: 10 }, "first");

    intent.leave();

    expect(intent.currentPosition()).toBeNull();
    expect(intent.move({ clientX: 50, clientY: 50 }, "third")).toBe("third");
  });
});
