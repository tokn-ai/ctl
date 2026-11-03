import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { basename, join } from "node:path";
import { tmpdir } from "node:os";
import test from "node:test";
import { probeComponent } from "../shared/component-probe.mts";
import { maxComponentMetadata } from "../shared/protocol-contract.mts";

test("returns the staged probe's stdout and stderr", async () => {
  const output = await probeComponent(process.execPath, ["-e", `process.stdout.write('{"version":"test"}'); process.stderr.write("note");`]);
  assert.deepEqual(output, { stdout: '{"version":"test"}', stderr: "note" });
});

test("records a slow successful probe without changing returned metadata", async (t) => {
  const times = [0, 5_123];
  t.mock.method(performance, "now", () => times.shift() ?? 5_123);
  const warnings = t.mock.method(console, "warn", () => {});
  const output = await probeComponent(process.execPath, ["-e", 'process.stdout.write("metadata")'], {
    target: "fixture-target", timeout_ms: 30_000,
  });
  assert.deepEqual(output, { stdout: "metadata", stderr: "" });
  assert.equal(warnings.mock.callCount(), 1);
  assert.match(String(warnings.mock.calls[0].arguments[0]), /Slow component metadata probe: .*target=fixture-target .*elapsed_ms=5123 timeout_ms=30000/);
});

test("reports a nonzero exit with bounded diagnostic stderr and the original cause", async () => {
  const script = 'process.stderr.write("probe failure: " + "x".repeat(4_000) + "END"); process.exitCode = 7;';
  await assert.rejects(probeComponent(process.execPath, ["-e", script], {
    env: { ...process.env, FIXTURE_VALUE: "unprinted-env-value" }, target: "fixture-target", timeout_ms: 2_000,
  }), (error: Error) => {
    assert.match(error.message, /exited unsuccessfully/);
    assert.ok(error.message.includes(`executable=${basename(process.execPath)} target=fixture-target host=${process.platform}/${process.arch}`));
    assert.match(error.message, /elapsed_ms=\d+ timeout_ms=2000 code=7 signal=null killed=false/);
    assert.match(error.message, /stderr="probe failure: x+" \(truncated\)$/);
    assert.doesNotMatch(error.message, /END|unprinted-env-value/);
    assert.ok(error.message.length < 3_000);
    assert.ok(error.cause instanceof Error);
    return true;
  });
});

test("distinguishes a child signal from a deadline kill", { skip: process.platform === "win32" }, async () => {
  await assert.rejects(probeComponent(process.execPath, ["-e", 'process.kill(process.pid, "SIGTERM");']), (error: Error) => {
    assert.match(error.message, /terminated by signal/);
    assert.match(error.message, /signal=SIGTERM killed=false/);
    return true;
  });
});

test("the configured deadline terminates a hung probe even if it ignores SIGTERM", { timeout: 5_000 }, async () => {
  await assert.rejects(probeComponent(process.execPath, ["-e", 'process.on("SIGTERM", () => {}); setInterval(() => {}, 1000);'], {
    env: { ...process.env, CTL_COMPONENT_PROBE_TIMEOUT_MS: "250" },
  }), (error: Error) => {
    assert.match(error.message, /timed out/);
    assert.match(error.message, /timeout_ms=250/);
    assert.match(error.message, /killed=true/);
    return true;
  });
});

test("the deadline includes inherited pipes after the component has exited", { timeout: 5_000 }, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "component-probe-pipes-"));
  const pid_file = join(directory, "holder.pid");
  t.after(async () => {
    try {
      const pid = Number(await readFile(pid_file, "utf8"));
      process.kill(pid, "SIGKILL");
    } catch (error) {
      if (!["ENOENT", "ESRCH"].includes((error as NodeJS.ErrnoException).code ?? "")) throw error;
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });
  const script = `
    const { spawn } = require("node:child_process");
    const holder = spawn(process.execPath, ["-e", "setTimeout(() => {}, 2000)"], { stdio: ["ignore", 1, 2] });
    require("node:fs").writeFileSync(process.argv[1], String(holder.pid));
    holder.unref();
    process.stderr.write("probe exited");
  `;
  await assert.rejects(probeComponent(process.execPath, ["-e", script, pid_file], { timeout_ms: 500 }), (error: Error) => {
    assert.match(error.message, /timed out/);
    assert.match(error.message, /code=0 signal=null killed=false stderr="probe exited"/);
    return true;
  });
  // The probe must settle while the descendant still owns its write ends.
  process.kill(Number(await readFile(pid_file, "utf8")), 0);
});

test("reports a missing executable as a launch error", async () => {
  await assert.rejects(probeComponent(join(tmpdir(), "missing-component-fixture", "ctl-agent"), ["--component-info"]), (error: Error) => {
    assert.match(error.message, /could not launch: executable=ctl-agent/);
    assert.match(error.message, /code=ENOENT/);
    return true;
  });
});

test("enforces the metadata size bound regardless of the deadline", async () => {
  await assert.rejects(probeComponent(process.execPath, ["-e", `process.stdout.write("x".repeat(${maxComponentMetadata + 1}));`]), (error: Error) => {
    assert.match(error.message, /exceeded the 16384-byte output limit/);
    assert.match(error.message, /code=ERR_CHILD_PROCESS_STDIO_MAXBUFFER/);
    return true;
  });
});

test("rejects invalid timeout settings before launching the component", async () => {
  for (const value of ["", "0", "-1", "1.5", "abc", "2147483648"]) {
    await assert.rejects(probeComponent("missing-component-fixture", ["--component-info"], {
      env: { ...process.env, CTL_COMPONENT_PROBE_TIMEOUT_MS: value },
    }), /Invalid component probe timeout/);
  }
});
