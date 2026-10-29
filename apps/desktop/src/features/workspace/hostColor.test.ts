import { describe, expect, it, vi } from "vitest";
import type { SshConnectionTarget } from "../../lib/types";
import { HOST_COLOR_PALETTE, hostColor, hostIdColor, normalizeHostColor, randomHostColors } from "./hostColor";

describe("host colors", () => {
  it("reserves white for the local host in sessions and tasks", () => {
    expect(hostColor({ kind: "local" })).toBe("#ffffff");
    expect(hostIdColor("local")).toBe("#ffffff");
    expect(hostColor({ kind: "local" }, "#ff0000")).toBe("#ffffff");
    expect(hostIdColor("local", "#ff0000")).toBe("#ffffff");
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

  it("resolves saved hex colors consistently and falls back for invalid values", () => {
    const target: SshConnectionTarget = { kind: "ssh", host_id: "build", destination: "build-server" };
    expect(hostColor(target, "#A1B2C3")).toBe("#a1b2c3");
    expect(hostIdColor("build", "#A1B2C3")).toBe(hostColor(target, "#A1B2C3"));
    for (const invalid of ["", "red", "#fff", "#gggfff", " #ffffff", "#ffffff00"]) {
      expect(hostColor(target, invalid)).toBe(hostColor(target));
      expect(hostIdColor("build", invalid)).toBe(hostIdColor("build"));
    }
    expect(normalizeHostColor(null)).toBeUndefined();
    expect(normalizeHostColor(123456)).toBeUndefined();
  });

  it("offers valid distinct preset and random colors", () => {
    const colors = HOST_COLOR_PALETTE.map(({ color }) => color);
    expect(colors.every((color) => normalizeHostColor(color) === color)).toBe(true);
    expect(new Set(colors).size).toBe(colors.length);

    const random = vi.spyOn(Math, "random").mockReturnValue(0.5);
    try {
      const suggestions = randomHostColors();
      expect(suggestions).toHaveLength(4);
      expect(new Set(suggestions).size).toBe(4);
      expect(suggestions.every((color) => normalizeHostColor(color) === color)).toBe(true);
      random.mockReturnValue(0.1);
      expect(randomHostColors()).not.toEqual(suggestions);
    } finally {
      random.mockRestore();
    }
  });
});
