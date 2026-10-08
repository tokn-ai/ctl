import { randomInt } from "node:crypto";

export function chooseDesktopDevPort(): number {
  // Leave room for Vite to retry occupied ports before reaching port 65535.
  return randomInt(49152, 60000);
}
