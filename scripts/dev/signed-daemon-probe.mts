import { lstat } from "node:fs/promises";
import { createConnection } from "node:net";

import { helperProtocol, lifecycleOffer, parseHelperComponent, type ProtocolInfo } from "../shared/protocol-contract.mts";

const maxFrameSize = 64 * 1024;

export async function inspectDaemon(socket_path: string, timeout_ms: number): Promise<ProtocolInfo | undefined> {
  try {
    const metadata = await lstat(socket_path);
    if (!metadata.isSocket() || metadata.uid !== process.getuid?.()) {
      throw new Error("signed ctld endpoint is not an owned Unix socket; no daemon was changed");
    }
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw error;
  }
  return new Promise((resolve, reject) => {
    const connection = createConnection(socket_path);
    let received = Buffer.alloc(0);
    let finished = false;
    const finish = (error?: Error, protocol?: ProtocolInfo): void => {
      if (finished) return;
      finished = true;
      clearTimeout(timer);
      connection.destroy();
      if (error) reject(error); else resolve(protocol);
    };
    // A deadline, not an idle timeout: a malformed owner cannot extend startup
    // indefinitely by sending one byte at a time.
    const timer = setTimeout(() => finish(new Error("existing signed ctld did not answer its lifecycle query in time; no daemon was changed")), timeout_ms);
    connection.once("error", (error: NodeJS.ErrnoException) => {
      // Normal ctld bootstrap owns stale-socket recovery. The supervisor never
      // unlinks a socket itself, including after a startup race.
      if (error.code === "ENOENT" || error.code === "ECONNREFUSED") finish();
      else finish(new Error("existing signed ctld socket could not be contacted; no daemon was changed"));
    });
    connection.once("end", () => finish(new Error("existing signed ctld closed without a valid lifecycle response; no daemon was changed")));
    connection.once("connect", () => {
      const payload = Buffer.from(JSON.stringify({ type: "ctld_inspect", protocol: lifecycleOffer }));
      const header = Buffer.alloc(4);
      header.writeUInt32BE(payload.length);
      connection.write(Buffer.concat([header, payload]));
    });
    connection.on("data", (chunk) => {
      if (received.length + chunk.length > maxFrameSize + 4) {
        finish(new Error("existing signed ctld returned an oversized lifecycle response; no daemon was changed"));
        return;
      }
      received = Buffer.concat([received, chunk]);
      if (received.length < 4) return;
      const length = received.readUInt32BE();
      if (length === 0 || length > maxFrameSize) {
        finish(new Error("existing signed ctld returned an invalid lifecycle frame; no daemon was changed"));
        return;
      }
      if (received.length < length + 4) return;
      try {
        const response = JSON.parse(received.subarray(4, length + 4).toString("utf8"));
        const protocols = parseHelperComponent(JSON.stringify(response?.info?.binary)).protocols;
        const protocol = helperProtocol(protocols);
        const lifecycle = helperProtocol(protocols, "ctld_lifecycle");
        if (response?.type !== "ctld_info" || response.protocol_version !== lifecycleOffer.version ||
          typeof response.info.instance_id !== "string" || response.info.instance_id.length === 0 || response.info.instance_id.length > 128 ||
          response.info.binary.protocol_version !== protocol.version || response.info.binary.lifecycle_protocol_version !== lifecycle.version ||
          !lifecycle.supported_versions.includes(lifecycleOffer.version)) {
          throw new Error("invalid response");
        }
        finish(undefined, protocol);
      } catch {
        finish(new Error("existing signed ctld returned an invalid lifecycle response; no daemon was changed"));
      }
    });
  });
}
