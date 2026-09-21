import { describe, expect, it } from "vitest";
import { hostFromTarget, hostTarget } from "../../features/workspace/workspaceModel";
import type { TailscaleDevice } from "../../lib/types";
import { hostSelectorChoices, tailscaleDeviceDetail, VIRTUAL_SSH_GROUP, VIRTUAL_TAILSCALE_GROUP } from "./hostChoices";

const device: TailscaleDevice = {
  node_id: "n123", name: "builder", dns_name: "builder.tailnet.ts.net",
  addresses: ["100.64.0.2"], online: true, os: "linux",
};

describe("host selector ownership groups", () => {
  it("groups saved and virtual hosts consistently even when their target order is mixed", () => {
    const virtual = {
      ...hostFromTarget({ kind: "ssh", host_id: "ssh-config:build", destination: "build" }),
      source: "ssh_config" as const,
    };
    const saved = hostFromTarget({ kind: "ssh", host_id: "saved", destination: "office" });
    const customized = { ...virtual, host_id: "ssh-config:custom", name: "My machine", source: "saved" as const };
    const choices = hostSelectorChoices([
      hostTarget(virtual, []), { kind: "local" }, hostTarget(saved, []), hostTarget(customized, []),
    ], [virtual, saved, customized]);
    expect(choices).toEqual([
      { id: "local", label: "local", group: "This machine" },
      { id: "host:saved", label: "office", group: "Saved hosts" },
      { id: "host:ssh-config:custom", label: "My machine", group: "Saved hosts" },
      { id: "host:ssh-config:build", label: "build", group: VIRTUAL_SSH_GROUP },
    ]);
  });

  it("groups Tailscale projections separately and moves customized hosts into saved hosts", () => {
    const virtual = {
      ...hostFromTarget({ kind: "ssh", host_id: "tailscale:n123", destination: "builder" }),
      source: "tailscale" as const,
      tailscale_device: device,
    };
    const customized = { ...virtual, host_id: "tailscale:n456", name: "My builder", source: "saved" as const };
    expect(hostSelectorChoices([hostTarget(virtual, []), hostTarget(customized, [])], [virtual, customized])).toEqual([
      { id: "host:tailscale:n456", label: "My builder", group: "Saved hosts", detail: "Online · linux · builder.tailnet.ts.net" },
      { id: "host:tailscale:n123", label: "builder", group: VIRTUAL_TAILSCALE_GROUP, detail: "Online · linux · builder.tailnet.ts.net" },
    ]);
  });

  it("shows offline and unknown device states without treating them as SSH connection status", () => {
    expect(tailscaleDeviceDetail({ ...device, online: false })).toBe("Offline · linux · builder.tailnet.ts.net");
    expect(tailscaleDeviceDetail({ ...device, online: null, dns_name: null, os: null })).toBe("Status unknown · 100.64.0.2");
  });

  it.each([false, null, undefined])("hides virtual Tailscale devices without online status while retaining saved hosts (%s)", (online) => {
    const virtual = {
      ...hostFromTarget({ kind: "ssh", host_id: "tailscale:n123", destination: "builder" }),
      source: "tailscale" as const,
      ...(online === undefined ? {} : { tailscale_device: { ...device, online } }),
    };
    const saved = { ...virtual, host_id: "saved", source: "saved" as const };
    const choices = hostSelectorChoices([hostTarget(virtual, []), hostTarget(saved, [])], [virtual, saved]);
    expect(choices).toHaveLength(1);
    expect(choices[0]).toMatchObject({ id: "host:saved", label: "builder", group: "Saved hosts" });
  });
});
