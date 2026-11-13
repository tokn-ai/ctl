// Runs only against the isolated SSH server owned by windows-ssh.ps1.
// The parent passes fixture credentials on stdin, never argv or environment.
import { spawn, spawnSync } from "node:child_process";
import { connect } from "node:net";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { text } from "node:stream/consumers";
import type { Writable } from "node:stream";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

/** Complete the write before closing the underlying pipe, including Bun sinks. */
export async function endAndClose(stream: Writable, input: string | Buffer): Promise<void> {
  return new Promise((resolve, reject) => {
    stream.once("error", reject);
    stream.once("close", resolve);
    stream.end(input, () => { stream.destroy(); });
  });
}

async function connectPipe(path: string): Promise<import("node:net").Socket> {
  const deadline = Date.now() + 4000;
  while (true) {
    try {
      return await new Promise<import("node:net").Socket>((resolve, reject) => {
        const connection = connect(path);
        connection.once("error", reject);
        connection.once("connect", () => { connection.removeListener("error", reject); resolve(connection); });
      });
    } catch (error) {
      if (Date.now() >= deadline) throw error;
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
  }
}

async function main() {
  const [ctl, destination] = process.argv.slice(2);
  const { user, password } = JSON.parse(await text(process.stdin)) as { user: string; password: string };

  async function invoke(label: string, supplied: string, input: string, command: string[]) {
    const pipe = `\\\\.\\pipe\\ctl-password-${randomUUID()}`;
    const child = spawn(ctl, ["--destination", destination, "--user", user,
      "--password-pipe", pipe, "--remote-platform", "windows", ...command],
      { stdio: ["pipe", "pipe", "pipe"] });
    let stdout = "", stderr = "";
    const state = {
      case_name: label,
      phase: "credential_connect",
      credential_finished: false,
      credential_closed: false,
      stdin_finished: false,
      stdin_closed: false,
      exit_seen: false,
    };
    child.stdin.once("finish", () => { state.stdin_finished = true; });
    child.stdin.once("close", () => { state.stdin_closed = true; });
    child.once("exit", () => { state.exit_seen = true; });
    child.stdout.on("data", (bytes) => { stdout += bytes.toString(); });
    child.stderr.on("data", (bytes) => { stderr += bytes.toString(); });
    const exited = new Promise<number | null>((resolve, reject) => {
      child.once("error", reject); child.once("close", resolve);
    });
    let terminationRequested = false;
    function terminateTree() {
      if (terminationRequested || child.exitCode !== null || child.signalCode !== null || child.pid === undefined) return;
      terminationRequested = true;
      // Enumerate the owned descendants before terminating ctl: killing ctl alone
      // can orphan ssh.exe while it still holds our captured output handles.
      const result = spawnSync("taskkill.exe", ["/PID", String(child.pid), "/T", "/F"], {
        stdio: "ignore", windowsHide: true, timeout: 5000,
      });
      if (result.status !== 0) child.kill();
    }
    let timer: ReturnType<typeof setTimeout> | undefined;
    const deadline = new Promise<never>((_, reject) => {
      timer = setTimeout(() => {
        // Do not include raw command output: it can contain a disclosed credential.
        const diagnostic = JSON.stringify({ ...state, stdout_bytes: Buffer.byteLength(stdout), stderr_bytes: Buffer.byteLength(stderr), exit_code: child.exitCode, signal_code: child.signalCode });
        terminateTree();
        reject(new Error(`Caller-auth invocation timed out: ${diagnostic}`));
      }, 30000);
    });
    let socket: import("node:net").Socket | undefined;
    try {
      const invocation = async () => {
        socket = await connectPipe(pipe);
        socket.on("error", () => {});
        socket.once("finish", () => { state.credential_finished = true; });
        socket.once("close", () => { state.credential_closed = true; });
        state.phase = "credential_close";
        await endAndClose(socket, Buffer.from(supplied));
        state.phase = "command_stdin_close";
        await endAndClose(child.stdin, input);
        state.phase = "command_exit";
        const code = await exited;
        assert(![password, supplied].some((secret) => stdout.includes(secret) || stderr.includes(secret)), "credential disclosed");
        return { code, stdout, stderr };
      };
      return await Promise.race([invocation(), deadline]);
    } finally {
      clearTimeout(timer);
      socket?.destroy();
      child.stdin.destroy();
      terminateTree();
      child.stdout.destroy();
      child.stderr.destroy();
    }
  }

  // Keep cmd.exe /C expressions in one argument, as in the Windows SSH fixture.
  // more.com requires a console; findstr.exe supports redirected SSH stdin.
  const streamed = await invoke("streamed stdin", password, "CALLER_STDIN\r\n", ["exec", "--", "cmd.exe", "/D", "/C", "findstr.exe CALLER_STDIN"]);
  assert.equal(streamed.code, 0, streamed.stderr);
  assert(streamed.stdout.includes("CALLER_STDIN"));
  const status = await invoke("exit status", password, "", ["exec", "--", "cmd.exe", "/D", "/C", "exit /b 23"]);
  assert.equal(status.code, 23, status.stderr);
  const shell = await invoke("plain shell", password, "echo CALLER_SHELL\r\nexit 19\r\n", ["shell", "--plain"]);
  assert.equal(shell.code, 19, shell.stderr);
  assert(shell.stdout.includes("CALLER_SHELL"));
  const incorrect = await invoke("incorrect password", `wrong-${randomUUID()}`, "", ["exec", "--", "cmd.exe", "/D", "/C", "exit /b 0"]);
  assert.equal(incorrect.code, 255, incorrect.stderr);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  await main();
}
