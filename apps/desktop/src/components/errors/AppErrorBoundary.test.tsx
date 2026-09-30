import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { AppCrashFallback, AppErrorBoundary } from "./AppErrorBoundary";

describe("AppErrorBoundary", () => {
  it("turns a render exception into recoverable boundary state", () => {
    const error = new Error("input update failed");

    expect(AppErrorBoundary.getDerivedStateFromError(error)).toEqual({ error });
  });

  it("explains that sessions survive and offers a reload", () => {
    const markup = renderToStaticMarkup(
      <AppCrashFallback error={new Error("input update failed")} />,
    );

    expect(markup).toContain('role="alert"');
    expect(markup).toContain("Your rmux sessions are still running.");
    expect(markup).toContain("input update failed");
    expect(markup).toContain("Reload rmux");
  });
});
