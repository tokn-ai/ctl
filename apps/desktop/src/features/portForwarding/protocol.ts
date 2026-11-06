import type { PortForwardProtocol } from "../../lib/types";

export function portForwardProtocolLabel(protocol: PortForwardProtocol = "tcp"): string {
  return protocol === "any" ? "TCP/UDP" : protocol.toUpperCase();
}
