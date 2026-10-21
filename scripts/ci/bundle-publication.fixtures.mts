import { createHash } from "node:crypto";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { agentTargets } from "../shared/agent-bundle.mts";
import { assembleAgentBundleSet } from "./assemble-agent-bundle-set.mts";
import { archiveEntriesFixture, tarFixture } from "./agent-bundle.fixtures.mts";
import type { Publication } from "./stage-bundle-pages.mts";

export function publicationFixture(run_number: number, channel: "dev" | "stable" = "dev", app_version = "0.1.0", git_revision = `${run_number.toString(16).padStart(12, "0")}${"a".repeat(28)}`): Publication {
  const bundle_id = channel === "stable" ? app_version : `${app_version}-dev.${git_revision.slice(0, 12)}`;
  const run_id = String(1000 + run_number);
  return { channel, app_version, bundle_id, git_revision, run_id, run_number, run_attempt: 1, publication_id: `${bundle_id}-${run_id}-1` };
}

export async function componentSetFixture(root: string, publication: Publication): Promise<string> {
  const { app_version, bundle_id, git_revision } = publication;
  const identity = { app_version, bundle_id, git_revision };
  const input = join(root, `source-${publication.run_id}-${publication.run_attempt}`);
  await mkdir(input);
  const directory = join(root, `incoming-${publication.run_id}-${publication.run_attempt}`);
  await mkdir(directory);
  for (const target of agentTargets) {
    const name = `ctl-agent-bundle-${identity.bundle_id}-${target}.tar.gz`;
    const archive = tarFixture(archiveEntriesFixture(target, identity));
    await writeFile(join(input, name), archive);
    await writeFile(join(input, `${name}.sha256`), `${createHash("sha256").update(archive).digest("hex")}  ${name}\n`);
  }
  await assembleAgentBundleSet(identity, input, directory);
  return directory;
}
