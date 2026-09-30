import { buildDaemons, DaemonBuildError } from "./daemon-build.mts";

try {
  await buildDaemons(process.argv.slice(2), process.cwd(), process.env);
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
