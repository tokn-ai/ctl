import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { SshHostPicker } from "./SshHostPicker";

describe("SshHostPicker", () => {
  it("renders inactive SSH config host suggestions", () => {
    const markup = renderToStaticMarkup(
      <SshHostPicker
        suggestions={["rmux-docker", "lab"]}
        warning={null}
        onAddHost={vi.fn(() => true)}
        onClose={vi.fn()}
      />,
    );

    expect(markup).toContain("From SSH config");
    expect(markup).toContain('aria-label="Add rmux-docker"');
    expect(markup).toContain('aria-label="Add lab"');
    expect(markup).not.toContain("SSH config suggestions may be incomplete");
  });

  it("shows partial-discovery warnings without hiding suggestions", () => {
    const markup = renderToStaticMarkup(
      <SshHostPicker
        suggestions={["workstation"]}
        warning="could not read one included file"
        onAddHost={vi.fn(() => true)}
        onClose={vi.fn()}
      />,
    );

    expect(markup).toContain('aria-label="Add workstation"');
    expect(markup).toContain("SSH config suggestions may be incomplete");
    expect(markup).toContain("could not read one included file");
  });
});
