import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test, { type TestContext } from "node:test";
import {
  findProvisioningProfile, openProvisioningProject, prepareProvisioningProfile,
  type MacosCommandRunner, type ProvisioningOptions,
} from "./macos-provisioning.mts";

const identifier = "TEAM123ABC.dev.tokn-ai.ctl.ctld";
interface Profile { application: string; expiration: string }

async function fixture(t: TestContext): Promise<{
  options: ProvisioningOptions; profiles: Map<string, Profile>; calls: string[][];
}> {
  const root = await mkdtemp(join(tmpdir(), "ctld-provisioning-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const profiles = new Map<string, Profile>();
  const calls: string[][] = [];
  const run: MacosCommandRunner = async (command, args) => {
    calls.push([command, ...args]);
    if (command === "security") {
      const profile = profiles.get(args[args.indexOf("-i") + 1]!);
      if (!profile) throw new Error("unreadable fixture profile");
      await writeFile(args[args.indexOf("-o") + 1]!, JSON.stringify(profile));
    } else if (command === "/usr/libexec/PlistBuddy") {
      const profile = JSON.parse(await readFile(args.at(-1)!, "utf8")) as Profile;
      return { stdout: args[1]?.endsWith(":ExpirationDate") ? profile.expiration : profile.application, stderr: "" };
    } else if (command === "xcodebuild") {
      for (const profile of profiles.values()) profile.expiration = "2099-01-01T00:00:00Z";
    } else if (command !== "open") {
      throw new Error(`unexpected fixture command ${command}`);
    }
    return { stdout: "", stderr: "" };
  };
  return {
    options: {
      repository_root: root, target_directory: join(root, "target"),
      home_directory: join(root, "home"), env: {}, run, now: new Date("2026-10-02T00:00:00Z"),
    }, profiles, calls,
  };
}

async function addProfile(input: Awaited<ReturnType<typeof fixture>>, relative: string, profile: Profile): Promise<string> {
  const path = join(input.options.home_directory!, "Library/Developer/Xcode/UserData/Provisioning Profiles", relative);
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, "fixture CMS profile");
  input.profiles.set(path, profile);
  return path;
}

test("shared profile discovery selects the newest valid exact helper profile", async (t) => {
  const input = await fixture(t);
  await addProfile(input, "expired.profile", { application: identifier, expiration: "2026-01-01T00:00:00Z" });
  await addProfile(input, "unrelated.profile", { application: "TEAM123ABC.dev.other.app", expiration: "2099-01-01T00:00:00Z" });
  await addProfile(input, "old.profile", { application: identifier, expiration: "2027-01-01T00:00:00Z" });
  const newest = await addProfile(input, "nested/new.profile", { application: identifier, expiration: "2028-01-01T00:00:00Z" });
  assert.equal((await findProvisioningProfile(input.options))?.path, newest);
  assert.equal(input.calls.some((call) => call[0] === "xcodebuild"), false);
});

test("an explicit profile override is exclusive rather than replaced by a discovered profile", async (t) => {
  const input = await fixture(t);
  await addProfile(input, "new.profile", { application: identifier, expiration: "2099-01-01T00:00:00Z" });
  input.options.env = { CTLD_PROVISIONING_PROFILE: join(input.options.repository_root, "missing.profile") };
  assert.equal(await findProvisioningProfile(input.options), undefined);
  await assert.rejects(prepareProvisioningProfile(input.options), /pnpm ctld:provision/);
  assert.equal(input.calls.some((call) => call[0] === "xcodebuild"), false);
});

test("configured Personal Team provisioning automatically refreshes an expired profile", async (t) => {
  const input = await fixture(t);
  const expired = await addProfile(input, "expired.profile", { application: identifier, expiration: "2026-01-01T00:00:00Z" });
  const project = join(input.options.target_directory, "ctld-provisioning/ctld-provisioning.xcodeproj");
  await mkdir(project, { recursive: true });
  await writeFile(join(project, "project.pbxproj"), "DEVELOPMENT_TEAM = TEAM123ABC;");
  assert.equal((await prepareProvisioningProfile(input.options)).path, expired);
  const refresh = input.calls.find((call) => call[0] === "xcodebuild")!;
  assert.ok(refresh.includes("-allowProvisioningUpdates") && refresh.includes("-allowProvisioningDeviceRegistration"));
  assert.equal(refresh[refresh.indexOf("-project") + 1], project);
});

test("provisioning opens a copied Xcode project and preserves the saved team selection", async (t) => {
  const input = await fixture(t);
  const template = join(input.options.repository_root, "scripts/dev/macos/ctld-provisioning/ctld-provisioning.xcodeproj");
  await mkdir(template, { recursive: true });
  await writeFile(join(template, "project.pbxproj"), 'DEVELOPMENT_TEAM = "";');
  await openProvisioningProject(input.options);
  const project = join(input.options.target_directory, "ctld-provisioning/ctld-provisioning.xcodeproj/project.pbxproj");
  await writeFile(project, "DEVELOPMENT_TEAM = TEAM123ABC;");
  await openProvisioningProject(input.options);
  assert.equal(await readFile(project, "utf8"), "DEVELOPMENT_TEAM = TEAM123ABC;");
  assert.equal(await readFile(join(template, "project.pbxproj"), "utf8"), 'DEVELOPMENT_TEAM = "";');
  assert.equal(input.calls.filter((call) => call[0] === "open").length, 2);
});
