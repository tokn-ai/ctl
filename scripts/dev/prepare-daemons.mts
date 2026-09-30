import { buildDaemons, DaemonBuildError } from "./daemon-build.mts";
import { requestPreparation } from "./daemon-preparation.mts";

try {
  const artifacts = await buildDaemons(process.argv.slice(2), process.cwd(), process.env);
  const supervisor = process.env.RMUX_DEV_DAEMON_SUPERVISOR;
  if (supervisor) {
    await requestPreparation(supervisor, artifacts.ctld);
  }
} catch (error) {
  if (error instanceof DaemonBuildError && error.compilation_failed) {
    // Tauri keeps watching after exit 101 only when the last diagnostic contains
    // "could not compile", matching Cargo's normal compilation-failure output.
    console.error(`error: could not compile local daemons (${error.message})`);
    process.exitCode = 101;
  } else {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}
