import { describe, expect, it } from "vitest";
import { errorCode, errorMessage } from "./errors";

describe("command errors", () => {
  it("preserves structured Tauri command error fields", () => {
    const error = {
      code: "session_not_found",
      message: "the session is already gone",
    };

    expect(errorCode(error)).toBe("session_not_found");
    expect(errorMessage(error)).toBe("the session is already gone");
  });

  it("handles ordinary errors and unknown rejection values", () => {
    expect(errorCode(new Error("failed"))).toBeNull();
    expect(errorMessage(new Error("failed"))).toBe("failed");
    expect(errorMessage(null)).toBe("An unexpected rmux error occurred.");
  });
});
