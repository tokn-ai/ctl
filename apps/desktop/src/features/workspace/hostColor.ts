import type { ConnectionTarget } from "../../lib/types";
import { targetKey } from "../targets/targets";

const LOCAL_HOST_COLOR = "#ffffff";

function identityColor(identity: string): string {
  // FNV-1a keeps a host's accent stable without persisting presentation state.
  let hash = 2166136261;
  for (let index = 0; index < identity.length; index += 1) {
    hash = Math.imul(hash ^ identity.charCodeAt(index), 16777619);
  }
  const value = hash >>> 0;
  const hue = value % 360;
  // Use the upper bits too: matching hues need not produce identical accents.
  // Keep the whole range bright enough for the dark sidebar and tab surfaces.
  const saturation = 58 + ((value >>> 16) % 15);
  const lightness = 64 + ((value >>> 24) % 11);
  return `hsl(${hue} ${saturation}% ${lightness}%)`;
}

export function hostIdColor(host_id: string): string {
  return host_id === "local" ? LOCAL_HOST_COLOR : identityColor(`host:${host_id}`);
}

export function hostColor(target: ConnectionTarget): string {
  if (target.kind === "local") return LOCAL_HOST_COLOR;
  return target.host_id ? hostIdColor(target.host_id) : identityColor(targetKey(target));
}
