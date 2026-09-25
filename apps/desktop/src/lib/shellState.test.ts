import { describe, expect, it } from "vitest";
import { displayWorkingDirectory } from "./shellState";

describe("displayWorkingDirectory", () => {
  it("prefers the target-derived display path", () => {
    expect(
      displayWorkingDirectory({
        cwd: "/Users/me/project",
        cwd_display: "~/project",
      }),
    ).toBe("~/project");
  });

  it("falls back to cwd for an older daemon snapshot", () => {
    expect(displayWorkingDirectory({ cwd: "/work/project" })).toBe(
      "/work/project",
    );
  });
});
