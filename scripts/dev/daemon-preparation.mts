import { createConnection, createServer, type Server, type Socket } from "node:net";

const maximumMessageBytes = 16 * 1024;

function readMessage(socket: Socket): Promise<unknown> {
  return new Promise((resolve, reject) => {
    let contents = "";
    socket.setEncoding("utf8");
    socket.setTimeout(60_000, () => socket.destroy(new Error("daemon preparation timed out")));
    socket.on("error", reject);
    socket.on("end", () => reject(new Error("daemon preparation closed without a response")));
    socket.on("data", (chunk: string) => {
      contents += chunk;
      if (Buffer.byteLength(contents) > maximumMessageBytes) {
        socket.destroy(new Error("daemon preparation message is too large"));
        return;
      }
      const end = contents.indexOf("\n");
      if (end < 0) return;
      try {
        resolve(JSON.parse(contents.slice(0, end)));
      } catch (error) {
        reject(error);
      }
    });
  });
}

export async function requestPreparation(socket_path: string, executable: string): Promise<void> {
  const socket = createConnection(socket_path);
  try {
    const response = readMessage(socket);
    socket.write(`${JSON.stringify({ executable })}\n`);
    const message = await response;
    if (typeof message !== "object" || message === null || !("ok" in message)) {
      throw new Error("invalid daemon preparation response");
    }
    if (message.ok !== true) {
      throw new Error("error" in message ? String(message.error) : "daemon preparation failed");
    }
  } finally {
    socket.destroy();
  }
}

export async function servePreparation(
  socket_path: string,
  prepare: (executable: string) => Promise<void>,
): Promise<{ close: () => Promise<void> }> {
  const sockets = new Set<Socket>();
  const server: Server = createServer((socket) => {
    sockets.add(socket);
    socket.once("close", () => sockets.delete(socket));
    void (async () => {
      try {
        const message = await readMessage(socket);
        if (
          typeof message !== "object" || message === null ||
          !("executable" in message) || typeof message.executable !== "string"
        ) {
          throw new Error("invalid daemon preparation request");
        }
        await prepare(message.executable);
        socket.end(`${JSON.stringify({ ok: true })}\n`);
      } catch (error) {
        socket.end(`${JSON.stringify({ ok: false, error: String(error) })}\n`);
      }
    })();
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(socket_path, resolve);
  });
  return {
    close: async () => {
      for (const socket of sockets) socket.destroy();
      await new Promise<void>((resolve, reject) => {
        server.close((error) => error ? reject(error) : resolve());
      });
    },
  };
}
