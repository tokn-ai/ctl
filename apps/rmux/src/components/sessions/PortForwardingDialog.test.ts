import { describe, expect, it } from "vitest";
import {
  listenerScope,
  remoteHostForListener,
} from "./PortForwardingDialog";

describe("remote listener forwarding defaults", () => {
  it("connects wildcard listeners through the matching loopback family", () => {
    expect(remoteHostForListener("0.0.0.0")).toBe("127.0.0.1");
    expect(remoteHostForListener("::")).toBe("::1");
    expect(remoteHostForListener("10.0.0.5")).toBe("10.0.0.5");
  });

  it("labels listener exposure without process inspection", () => {
    expect(listenerScope("127.0.0.1")).toBe("Loopback");
    expect(listenerScope("::1")).toBe("Loopback");
    expect(listenerScope("0.0.0.0")).toBe("All interfaces");
    expect(listenerScope("192.168.1.2")).toBe("Specific interface");
  });
});
