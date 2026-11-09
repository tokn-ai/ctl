import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import type { DevelopmentInstallation } from "./ctlup-development.mts";

/** A private executable manager fixture, including the stdin-held lease. */
export async function writeDevelopmentManager(directory: string, executables?: Record<string, string>, target = "fixture-host") {
  const manager_executable = path.join(directory, "bootstrap-ctlup");
  const bin = path.join(directory, "managed", "bin");
  await mkdir(bin, { recursive: true });
  const installation: DevelopmentInstallation = {
    schema_version: 1,
    bundle_id: "a".repeat(64),
    target_triple: target,
    executables: Object.fromEntries(["ctl-agent", "ctmuxd", "ctl-taskd", "ctld", "ctl", "ctmux", "ctlup"].map((name) => [name, executables?.[name] ?? path.join(bin, name)])) as DevelopmentInstallation["executables"],
    services_preserved: true,
  };
  await writeFile(manager_executable, `#!${process.execPath}
const fs = require("node:fs");
if(process.env.TEST_MANAGER_CALLS) fs.appendFileSync(process.env.TEST_MANAGER_CALLS, JSON.stringify({args:process.argv.slice(2),cwd:process.cwd(),executable:process.argv[1]})+"\\n");
if(process.env.TEST_MANAGER_FAILURE) { console.error(process.env.TEST_MANAGER_FAILURE); process.exit(1); }
console.log(${JSON.stringify(JSON.stringify(installation))});
process.stdin.resume();
process.stdin.once("end", () => process.exit(0));
`, { mode: 0o755 });
  await writeFile(path.join(directory, "strip"), `#!${process.execPath}\nprocess.exit(0);\n`, { mode: 0o755 });
  return { manager_executable, installation };
}
