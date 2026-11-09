// @vitest-environment node
import { createServer } from "node:http";
import { mkdir, mkdtemp, realpath, rename, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, expect, it, vi } from "vitest";
import { startDesktopFrontend } from "./server.mts";

const cleanup: (() => Promise<unknown>)[] = [];
afterEach(async () => {
  for (const close of cleanup.reverse()) await close();
  cleanup.length = 0;
});

async function fixture(dependencies: string[] = []) {
  const root = await realpath(await mkdtemp(join(tmpdir(), "ctl-frontend-test-")));
  cleanup.push(() => rm(root, { recursive: true, force: true }));
  await writeFile(join(root, "index.html"), "<h1>Frontend fixture</h1>");
  const dependency_plugin = fileURLToPath(new URL("./installed-dependencies.ts", import.meta.url));
  await writeFile(join(root, "vite.config.mjs"), `import { refreshInstalledDependencies } from ${JSON.stringify(dependency_plugin)};
export default {
    plugins: [refreshInstalledDependencies()],
    logLevel: "silent",
    optimizeDeps: { noDiscovery: true, include: ${JSON.stringify(dependencies)} },
    server: { port: 1430, strictPort: true, hmr: { host: "127.0.0.1", protocol: "ws" } },
  };`);
  return root;
}

async function start(root: string, initial_port?: number) {
  const frontend = await startDesktopFrontend({ app_directory: root, initial_port });
  cleanup.push(() => frontend.server.close());
  return frontend;
}

it("binds distinct live ports for concurrent launches and preserves an occupied listener", async () => {
  const root = await fixture();
  const occupied = createServer((_request, response) => response.end("Existing frontend"));
  await new Promise<void>((resolve, reject) => {
    occupied.once("error", reject);
    occupied.listen(0, "127.0.0.1", () => {
      occupied.off("error", reject);
      resolve();
    });
  });
  cleanup.push(() => new Promise<void>((resolve, reject) => occupied.close((error) => error ? reject(error) : resolve())));
  const address = occupied.address();
  if (!address || typeof address === "string") throw new Error("Missing occupied port");
  const first = await start(root, address.port);
  const second = await start(root, address.port);
  expect(first.url).not.toBe(second.url);
  expect(Number(new URL(first.url).port)).toBeGreaterThan(address.port);
  expect(await (await fetch(first.url)).text()).toContain("Frontend fixture");
  expect(await (await fetch(second.url)).text()).toContain("Frontend fixture");
  await first.server.restart();
  expect(first.server.resolvedUrls?.local[0]).toBe(first.url);
  expect(await (await fetch(`http://127.0.0.1:${address.port}`)).text()).toBe("Existing frontend");
});

it("rebuilds installed dependencies at the same URL and keeps watching after restarts", async () => {
  const root = await fixture(["fixture-dependency"]);
  const dependency = join(root, "node_modules/fixture-dependency");
  const lock_directory = join(root, "node_modules/.pnpm");
  await mkdir(dependency, { recursive: true });
  await mkdir(lock_directory, { recursive: true });
  await writeFile(join(root, "package.json"), JSON.stringify({ type: "module" }));
  await writeFile(join(root, "index.html"), '<script type="module" src="/entry.js"></script>');
  await writeFile(join(root, "entry.js"), 'import { version } from "fixture-dependency"; console.log(version);');
  await writeFile(join(dependency, "package.json"), JSON.stringify({
    name: "fixture-dependency",
    version: "1.0.0",
    type: "module",
    main: "index.js",
  }));
  await writeFile(join(dependency, "index.js"), 'export const version = "fixture version 1";');
  await writeFile(join(lock_directory, "lock.yaml"), "fixture_version: 1\n");
  const frontend = await start(root);
  const url = frontend.url;
  const optimized_dependency = async () => {
    try {
      const entry = await (await fetch(new URL("entry.js", url), { headers: { "Sec-Fetch-Dest": "script" } })).text();
      const dependency_url = entry.match(/from "([^"]+fixture-dependency[^"]+)"/)?.[1];
      return dependency_url ? (await fetch(new URL(dependency_url, url))).text() : entry;
    } catch {
      return "";
    }
  };
  await expect.poll(optimized_dependency, { timeout: 10_000 }).toContain("fixture version 1");
  // Vite can resolve a failed restart while retaining the previous server.
  const restart = vi.spyOn(frontend.server, "restart").mockResolvedValueOnce(undefined);
  for (const version of [2, 3]) {
    if (version === 3) await frontend.server.restart();
    await writeFile(join(dependency, "index.js"), `export const version = "fixture version ${version}";`);
    await writeFile(join(lock_directory, "lock.yaml.new"), `fixture_version: ${version}\n`);
    await rename(join(lock_directory, "lock.yaml.new"), join(lock_directory, "lock.yaml"));
    await expect.poll(optimized_dependency, { timeout: 10_000 }).toContain(`fixture version ${version}`);
    if (version === 2) expect(restart).toHaveBeenCalledTimes(2);
    expect(frontend.server.resolvedUrls?.local[0]).toBe(url);
  }
}, 30_000);

it("keeps HTTP and hot reload on the same port through Vite config reloads", async () => {
  const frontend = await start(await fixture());
  const url = frontend.url;
  const socket_url = new URL(url);
  socket_url.protocol = "ws:";
  socket_url.searchParams.set("token", frontend.server.config.webSocketToken);
  const socket = new WebSocket(socket_url, "vite-hmr");
  const connected = await new Promise<string>((resolve, reject) => {
    socket.addEventListener("message", (event) => resolve(String(event.data)), { once: true });
    socket.addEventListener("error", () => reject(new Error("HMR connection failed")), { once: true });
  });
  expect(JSON.parse(connected)).toEqual({ type: "connected" });
  const closed = new Promise<void>((resolve) => socket.addEventListener("close", () => resolve(), { once: true }));
  socket.close();
  await closed;
  await frontend.server.restart();
  expect(frontend.server.resolvedUrls?.local[0]).toBe(url);
  expect(await (await fetch(url)).text()).toContain("Frontend fixture");
  await frontend.server.close();
  await expect(fetch(url)).rejects.toThrow();
});
