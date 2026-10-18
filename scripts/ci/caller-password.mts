// Runs only against the isolated SSH server owned by windows-ssh.ps1.
// The parent passes fixture credentials on stdin, never argv or environment.
import { spawn } from "node:child_process";
import { connect } from "node:net";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
const [ctl, destination] = process.argv.slice(2);
const { user, password } = JSON.parse(await Bun.stdin.text()) as { user: string; password: string };

async function invoke(supplied: string, input: string, command: string[]) {
  const pipe = `\\\\.\\pipe\\ctl-password-${randomUUID()}`;
  const child = spawn(ctl, ["--destination", destination, "--user", user,
    "--password-pipe", pipe, "--remote-platform", "windows", ...command],
    { stdio: ["pipe", "pipe", "pipe"] });
  let stdout = "", stderr = "";
  child.stdout.on("data", (bytes) => { stdout += bytes.toString(); });
  child.stderr.on("data", (bytes) => { stderr += bytes.toString(); });
  const exited = new Promise<number | null>((resolve, reject) => {
    child.once("error", reject); child.once("close", resolve);
  });
  const timer = setTimeout(() => child.kill(), 30000);
  const deadline = Date.now() + 4000;
  while (true) {
    try {
      const socket = await new Promise<import("node:net").Socket>((resolve, reject) => {
        const connection = connect(pipe);
        connection.once("error", reject);
        connection.once("connect", () => { connection.removeListener("error", reject); resolve(connection); });
      });
      socket.on("error", () => {});
      socket.end(Buffer.from(supplied));
      break;
    } catch (error) {
      if (Date.now() >= deadline) { child.kill(); throw error; }
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
  }
  child.stdin.end(input);
  const code = await exited;
  clearTimeout(timer);
  assert(!stdout.includes(password) && !stderr.includes(password), "credential disclosed");
  return { code, stdout, stderr };
}

const streamed = await invoke(password, "CALLER_STDIN\r\n", ["exec", "--", "cmd", "/c", "more"]);
assert.equal(streamed.code, 0, streamed.stderr);
assert(streamed.stdout.includes("CALLER_STDIN"));
const status = await invoke(password, "", ["exec", "--", "cmd", "/c", "exit", "23"]);
assert.equal(status.code, 23, status.stderr);
const shell = await invoke(password, "echo CALLER_SHELL\r\nexit 19\r\n", ["shell", "--plain"]);
assert.equal(shell.code, 19, shell.stderr);
assert(shell.stdout.includes("CALLER_SHELL"));
const incorrect = await invoke(`wrong-${randomUUID()}`, "", ["exec", "--", "cmd", "/c", "exit", "0"]);
assert.equal(incorrect.code, 255, incorrect.stderr);
