import type { ConnectionTarget } from "../../lib/types";
import { targetKey } from "../targets/targets";

const LOCAL_HOST_COLOR = "#ffffff";

export const HOST_COLOR_PALETTE = [
  { name: "Coral", color: "#f18f8f" },
  { name: "Orange", color: "#f3ad76" },
  { name: "Amber", color: "#ebc66c" },
  { name: "Lime", color: "#bdd879" },
  { name: "Green", color: "#83d5a4" },
  { name: "Teal", color: "#75cfbd" },
  { name: "Cyan", color: "#7ec9e7" },
  { name: "Blue", color: "#91b7f2" },
  { name: "Indigo", color: "#aaa2ef" },
  { name: "Purple", color: "#c39aeb" },
  { name: "Pink", color: "#e79acb" },
  { name: "Rose", color: "#eb92ad" },
] as const;

export function normalizeHostColor(value: unknown): string | undefined {
  return typeof value === "string" && /^#[\da-f]{6}$/i.test(value)
    ? value.toLowerCase()
    : undefined;
}

function hslToHex(hue: number, saturation: number, lightness: number): string {
  const amplitude = saturation * Math.min(lightness, 1 - lightness);
  const channel = (offset: number) => {
    const position = (offset + hue / 30) % 12;
    const value = lightness - amplitude * Math.max(-1, Math.min(position - 3, 9 - position, 1));
    return Math.round(255 * value).toString(16).padStart(2, "0");
  };
  return `#${channel(0)}${channel(8)}${channel(4)}`;
}

/** Four bright suggestions, spread around the color wheel to avoid duplicates. */
export function randomHostColors(): string[] {
  const start = Math.random() * 360;
  return Array.from({ length: 4 }, (_, index) => hslToHex(
    (start + index * 137.508 + Math.random() * 24) % 360,
    0.58 + Math.random() * 0.14,
    0.64 + Math.random() * 0.1,
  ));
}

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

export function hostIdColor(host_id: string, color?: string): string {
  if (host_id === "local") return LOCAL_HOST_COLOR;
  return normalizeHostColor(color) ?? identityColor(`host:${host_id}`);
}

export function hostColor(target: ConnectionTarget, color?: string): string {
  if (target.kind === "local") return LOCAL_HOST_COLOR;
  const custom = normalizeHostColor(color);
  if (custom) return custom;
  return target.host_id ? hostIdColor(target.host_id) : identityColor(targetKey(target));
}
