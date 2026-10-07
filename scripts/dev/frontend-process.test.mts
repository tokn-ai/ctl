import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { startFrontendProcess } from "./frontend-process.mts";

async function fixture(t: TestContext, source: string) {
  const app_directory = await mkdtemp(join(tmpdir(), "ctl-frontend-process-test-"));
  t.after(() => rm(app_directory, { recursive: true, force: true }));
  const entry_path = join(app_directory, "frontend.mts");
  await writeFile(entry_path, source);
  return { app_directory, entry_path };
}

test("frontend readiness keeps its listener bound and shutdown handlers out of the launcher", async (t) => {
  const input = await fixture(t, `
    import { createServer } from "node:http";
    const server = createServer((_request, response) => response.end("Live frontend"));
    server.listen(0, "127.0.0.1", () => {
      process.send({ kind: "ready", url: "http://127.0.0.1:" + server.address().port + "/" });
    });
    process.once("SIGTERM", () => server.close(() => process.exit(0)));
  `);
  const signal_handlers = process.listenerCount("SIGTERM");
  const frontend = await startFrontendProcess(input);
  t.after(frontend.close);
  assert.equal(process.listenerCount("SIGTERM"), signal_handlers);
  assert.equal(await (await fetch(frontend.url)).text(), "Live frontend");
  await frontend.close();
  assert.deepEqual(await frontend.exited, { code: 0, signal: null });
  await assert.rejects(fetch(frontend.url));
});

test("a frontend startup exit fails promptly without leaving a child behind", async (t) => {
  const input = await fixture(t, "process.exit(7);");
  await assert.rejects(startFrontendProcess(input), /before reporting a URL \(status 7\)/);
});

test("invalid frontend readiness is rejected and its process is stopped", async (t) => {
  const input = await fixture(t, `
    process.send({ kind: "ready", url: "file:///invalid" });
    setInterval(() => {}, 1000);
  `);
  await assert.rejects(startFrontendProcess(input), /Invalid development URL/);
});
