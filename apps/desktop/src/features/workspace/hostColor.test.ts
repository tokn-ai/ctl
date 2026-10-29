import { describe, expect, it } from "vitest";
import type { SshConnectionTarget } from "../../lib/types";
import { hostColor, hostIdColor } from "./hostColor";

describe("host colors", () => {
  it("reserves white for the local host in sessions and tasks", () => {
    expect(hostColor({ kind: "local" })).toBe("#ffffff");
    expect(hostIdColor("local")).toBe("#ffffff");
    expect(hostIdColor("build-server")).not.toBe("#ffffff");
  });

  it("keeps a saved host's color across renames and route changes", () => {
    const direct: SshConnectionTarget = {
      kind: "ssh",
      host_id: "build-server",
      host_name: "Build",
      destination: "build.example.com",
      method_id: "direct",
    };
    const vpn: SshConnectionTarget = {
      ...direct,
      host_name: "Production build",
      destination: "100.64.0.2",
      method_id: "tailscale",
      tailscale_node_id: "node-2",
    };

    expect(hostColor(vpn)).toBe(hostColor(direct));
    expect(hostColor(direct)).toBe(hostIdColor("build-server"));
  });

  it("does not depend on catalog order or previously rendered hosts", () => {
    const ids = ["build-server", "staging-server", "development-server"];
    const original = new Map(ids.map((id) => [id, hostIdColor(id)]));
    const reordered = new Map([...ids].reverse().map((id) => [id, hostIdColor(id)]));

    expect(reordered).toEqual(original);
    expect(new Set(original.values()).size).toBe(ids.length);
  });

  it("distinguishes known hue collisions with saturation and lightness", () => {
    const first = hostIdColor("host-14");
    const second = hostIdColor("host-21");

    expect(first).toMatch(/^hsl\(49 /);
    expect(second).toMatch(/^hsl\(49 /);
    expect(first).not.toBe(second);
  });

  it("uses target identity for legacy hosts without a saved ID", () => {
    const target: SshConnectionTarget = { kind: "ssh", destination: "alice@legacy" };

    expect(hostColor({ ...target, host_name: "Renamed legacy host" })).toBe(hostColor(target));
    expect(hostColor({ ...target, destination: "bob@another-host" })).not.toBe(hostColor(target));
  });
});
