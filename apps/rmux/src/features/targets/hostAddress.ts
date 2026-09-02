export interface HostAddress {
  hostname: string;
  user: string | null;
  port: number | null;
}

/** Parse connection data, never a shell command or arbitrary SSH flags. */
export function parseHostAddress(value: string): HostAddress | null {
  const match =
    /^(?:([a-zA-Z0-9_.-]+)@)?(\[[a-fA-F0-9:]+\]|[a-zA-Z0-9_.-]+)(?::([0-9]+))?$/u.exec(
      value.trim(),
    );
  if (!match) return null;
  const hostname = match[2].replace(/^\[|\]$/gu, "");
  if (hostname.startsWith("-") || hostname === ".") return null;
  const port = match[3] ? Number(match[3]) : null;
  if (port !== null && (!Number.isInteger(port) || port < 1 || port > 65_535))
    return null;
  return { hostname, user: match[1] ?? null, port };
}
