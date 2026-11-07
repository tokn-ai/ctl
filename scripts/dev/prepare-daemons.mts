import { buildDaemons, DaemonBuildError } from "./daemon-build.mts";
import { prepareDevelopment, DevelopmentPreparationError } from "./ctlup-development.mts";

try {
  if (process.platform === "win32") {
    await buildDaemons(process.argv.slice(2), process.cwd(), process.env);
  } else {
    const prepared = await prepareDevelopment(process.argv.slice(2), process.cwd(), process.env);
    await prepared.close();
  }
} catch (error) {
  if ((error instanceof DaemonBuildError || error instanceof DevelopmentPreparationError) && error.compilation_failed) {
    console.error(`error: could not compile local components (${error.message})`);
    process.exitCode = 101;
  } else {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}
