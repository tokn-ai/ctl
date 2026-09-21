import { describe, expect, it } from "vitest";
import { hostFromTarget, hostTarget } from "../../features/workspace/workspaceModel";
import { hostSelectorChoices, VIRTUAL_SSH_GROUP } from "./hostChoices";

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
});
