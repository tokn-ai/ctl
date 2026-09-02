import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import {
  SshHostPicker,
  SshHostStorageChoice,
  parseSshHostDefinition,
} from "./SshHostPicker";

describe("SshHostPicker", () => {
  it("renders inactive SSH config host suggestions", () => {
    const markup = renderToStaticMarkup(
      <SshHostPicker
        suggestions={["rmux-docker", "lab"]}
        warning={null}
        onActivateHost={vi.fn(() => true)}
        onSaveHost={vi.fn(async () => undefined)}
        onClose={vi.fn()}
      />,
    );

    expect(markup).toContain("Already in SSH config");
    expect(markup).toContain('aria-label="Activate rmux-docker"');
    expect(markup).toContain('aria-label="Activate lab"');
    expect(markup).not.toContain("SSH config suggestions may be incomplete");
  });

  it("shows partial-discovery warnings without hiding suggestions", () => {
    const markup = renderToStaticMarkup(
      <SshHostPicker
        suggestions={["workstation"]}
        warning="could not read one included file"
        onActivateHost={vi.fn(() => true)}
        onSaveHost={vi.fn(async () => undefined)}
        onClose={vi.fn()}
      />,
    );

    expect(markup).toContain('aria-label="Activate workstation"');
    expect(markup).toContain("SSH config suggestions may be incomplete");
    expect(markup).toContain("could not read one included file");
  });

  it("normalizes host details before the storage step", () => {
    expect(
      parseSshHostDefinition({
        alias: " rmux-remote-test ",
        hostname: " 127.0.0.1 ",
        user: " rmux ",
        port: "2222",
        identity_file: " ~/.ssh/local.id_rsa ",
      }),
    ).toEqual({
      definition: {
        alias: "rmux-remote-test",
        hostname: "127.0.0.1",
        user: "rmux",
        port: 2222,
        identity_file: "~/.ssh/local.id_rsa",
      },
      error: null,
    });
  });

  it("rejects out-of-range ports before choosing storage", () => {
    const parsed = parseSshHostDefinition({
      alias: "invalid",
      hostname: "127.0.0.1",
      user: "",
      port: "0",
      identity_file: "",
    });

    expect(parsed.definition).toBeNull();
    expect(parsed.error).toBe("Port must be between 1 and 65535.");
  });

  it("offers SSH config and app-only persistence as separate choices", () => {
    const markup = renderToStaticMarkup(
      <SshHostStorageChoice
        definition={{
          alias: "rmux-remote-test",
          hostname: "127.0.0.1",
          user: "rmux",
          port: 2222,
          identity_file: "~/.ssh/local.id_rsa",
        }}
        saving={false}
        error={null}
        onSelect={vi.fn()}
        onBack={vi.fn()}
      />,
    );

    expect(markup).toContain("Where should this host be saved?");
    expect(markup).toContain("OpenSSH config");
    expect(markup).toContain("This app only");
  });
});
