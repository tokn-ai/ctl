import { execFile } from "node:child_process";

export interface BuildCommandContext {
  cwd: string;
  env: NodeJS.ProcessEnv;
  stream_output?: boolean;
  stream_stderr?: boolean;
  timeout_ms?: number;
}

export type BuildCommandRunner = (
  command: string, args: string[], context: BuildCommandContext,
) => Promise<{ stdout: string; stderr: string }>;

export const runBuildCommand: BuildCommandRunner = (command, args, context) =>
  new Promise((resolve, reject) => {
    const child = execFile(command, args, {
      cwd: context.cwd, env: context.env, timeout: context.timeout_ms,
      maxBuffer: args[0] === "--component-info" ? 16 * 1024 : 32 * 1024 * 1024,
    }, (error, stdout, stderr) => {
      if (error) reject(error);
      else resolve({ stdout, stderr });
    });
    if (context.stream_output) {
      child.stdout?.on("data", (chunk) => process.stdout.write(chunk));
    }
    if (context.stream_output || context.stream_stderr) {
      child.stderr?.on("data", (chunk) => process.stderr.write(chunk));
    }
  });
