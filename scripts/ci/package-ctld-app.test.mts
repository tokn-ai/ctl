import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const execute = promisify(execFile);
const repository_root = fileURLToPath(new URL("../../", import.meta.url));
const macos_only = { skip: process.platform !== "darwin" };
const team = "ABC123DEF4";
const identifier = "dev.tokn-ai.ctl.ctld";

interface Fixture {
  binary: string;
  output_app: string;
  env: NodeJS.ProcessEnv;
  calls_file: string;
}

async function executable(path: string, source: string): Promise<void> {
  await writeFile(path, source);
  await chmod(path, 0o700);
}

async function fixture(context: TestContext, profile_debug = false): Promise<Fixture> {
  const root = await mkdtemp(join(tmpdir(), "ctld-app-timestamp-test-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  const tools = join(root, "tools");
  await mkdir(tools);
  const certificate = Buffer.from("synthetic timestamp-test certificate; never used with Apple");
  const profile = join(root, "decoded-profile.plist");
  await writeFile(profile, `<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>Entitlements</key><dict>
    <key>com.apple.application-identifier</key><string>${team}.${identifier}</string>
    <key>com.apple.developer.team-identifier</key><string>${team}</string>
    <key>get-task-allow</key><${profile_debug ? "true" : "false"}/>
  </dict>
  <key>DeveloperCertificates</key><array><data>${certificate.toString("base64")}</data></array>
  <key>ProvisionsAllDevices</key><true/>
</dict></plist>`);
  const binary = join(root, "ctld");
  await executable(binary, "#!/bin/sh\nexit 0\n");
  const output_app = join(root, "output", "ctld.app");
  await mkdir(output_app, { recursive: true });
  await writeFile(join(output_app, "previous-marker"), "previous usable bundle");
  const calls_file = join(root, "codesign-calls");
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    PATH: `${tools}:/usr/bin:/bin:/usr/sbin:/sbin`,
    CTLD_PROVISIONING_PROFILE: profile,
    CTLD_REQUIRE_DISTRIBUTION_SIGNING: undefined,
    CTLD_SIGNING_TIMESTAMP: undefined,
    CTLD_SIGNING_IDENTITY_OUTPUT: undefined,
    CTLD_APP_TEST_PROFILE: profile,
    CTLD_APP_TEST_CERTIFICATE_HASH: createHash("sha1").update(certificate).digest("hex").toUpperCase(),
    CTLD_APP_TEST_CODESIGN_CALLS: calls_file,
    CTLD_APP_TEST_SIGNED_ENTITLEMENTS: join(root, "signed-entitlements.plist"),
    CTLD_APP_TEST_TIMESTAMP_OUTAGE: "true",
  };
  await executable(join(tools, "security"), `#!/bin/sh
set -eu
case "$1" in
  cms)
    while [ "$#" -gt 0 ]; do
      if [ "$1" = -o ]; then shift; /bin/cp "$CTLD_APP_TEST_PROFILE" "$1"; exit 0; fi
      shift
    done
    exit 2
    ;;
  find-identity)
    printf '  1) %s "${profile_debug ? "Apple Development" : "Developer ID Application"}: Fixture (${team})"\n     1 valid identities found\n' "$CTLD_APP_TEST_CERTIFICATE_HASH"
    ;;
  *) exit 2 ;;
esac
`);
  await executable(join(tools, "codesign"), `#!/bin/sh
set -eu
printf '%s\n' __CALL__ "$@" >> "$CTLD_APP_TEST_CODESIGN_CALLS"
case "$1" in
  --force)
    app=
    timestamp=
    for argument do
      app=$argument
      case "$argument" in --timestamp|--timestamp=none) timestamp=$argument ;; esac
    done
    if [ -z "$timestamp" ]; then echo 'fixture signing requires an explicit timestamp policy' >&2; exit 2; fi
    if [ "$timestamp" = --timestamp ] && [ "$CTLD_APP_TEST_TIMESTAMP_OUTAGE" = true ]; then
      echo 'fixture Apple timestamp service unavailable' >&2
      exit 71
    fi
    while [ "$#" -gt 0 ]; do
      if [ "$1" = --entitlements ]; then shift; /bin/cp "$1" "$CTLD_APP_TEST_SIGNED_ENTITLEMENTS"; fi
      shift
    done
    /bin/mkdir -p "$app/Contents/_CodeSignature"
    printf '%s\n' fixture-signature > "$app/Contents/_CodeSignature/CodeResources"
    ;;
  --verify) ;;
  -d)
    case "$*" in
      *--entitlements*) /bin/cat "$CTLD_APP_TEST_SIGNED_ENTITLEMENTS" ;;
      *--verbose=2*) printf '%s\n' 'TeamIdentifier=${team}' >&2 ;;
      *) exit 2 ;;
    esac
    ;;
  *) exit 2 ;;
esac
`);
  return { binary, output_app, env, calls_file };
}

async function packageApp(options: Fixture, env: NodeJS.ProcessEnv = {}): Promise<void> {
  await execute("/bin/sh", ["scripts/ci/package-ctld-app.sh", options.binary, options.output_app, "0.1.0"], {
    cwd: repository_root, env: { ...options.env, ...env }, timeout: 10_000,
  });
}

async function calls(options: Fixture): Promise<string[][]> {
  const log = await readFile(options.calls_file, "utf8").catch((error: NodeJS.ErrnoException) => {
    if (error.code === "ENOENT") return "";
    throw error;
  });
  return log.split("__CALL__\n").filter(Boolean).map((call) => call.trimEnd().split("\n"));
}

async function assertPreserved(options: Fixture): Promise<void> {
  assert.equal(await readFile(join(options.output_app, "previous-marker"), "utf8"), "previous usable bundle");
}

test("explicit local timestamp=none packages successfully during a timestamp service outage", macos_only, async (context) => {
  const options = await fixture(context, true);
  await packageApp(options, { CTLD_SIGNING_TIMESTAMP: "none" });
  const signing = (await calls(options)).find((args) => args[0] === "--force")!;
  assert.ok(signing.includes("--timestamp=none"));
  assert.ok(!signing.includes("--timestamp"));
  assert.equal(await readFile(join(options.output_app, "Contents", "MacOS", "ctld"), "utf8"), "#!/bin/sh\nexit 0\n");
  assert.equal(await readFile(join(options.output_app, "Contents", "embedded.provisionprofile"), "utf8"), await readFile(options.env.CTLD_PROVISIONING_PROFILE!, "utf8"));
});

test("default secure timestamp fails the outage and preserves the previous output", macos_only, async (context) => {
  const options = await fixture(context, true);
  await assert.rejects(packageApp(options), /fixture Apple timestamp service unavailable/);
  const signing = (await calls(options)).find((args) => args[0] === "--force")!;
  assert.ok(signing.includes("--timestamp"));
  assert.ok(!signing.includes("--timestamp=none"));
  await assertPreserved(options);
});

test("explicit secure timestamp remains required for distribution signing", macos_only, async (context) => {
  const options = await fixture(context);
  await packageApp(options, { CTLD_SIGNING_TIMESTAMP: "secure", CTLD_REQUIRE_DISTRIBUTION_SIGNING: "true", CTLD_APP_TEST_TIMESTAMP_OUTAGE: "false" });
  const recorded = await calls(options);
  assert.ok(recorded.find((args) => args[0] === "--force")!.includes("--timestamp"));
  assert.ok(recorded.some((args) => args.includes("--test-requirement")));
});

test("distribution signing rejects timestamp=none before invoking codesign and preserves output", macos_only, async (context) => {
  const options = await fixture(context);
  await assert.rejects(packageApp(options, { CTLD_SIGNING_TIMESTAMP: "none", CTLD_REQUIRE_DISTRIBUTION_SIGNING: "true" }), /distribution ctld signing requires a secure timestamp/);
  assert.deepEqual(await calls(options), []);
  await assertPreserved(options);
});

test("invalid timestamp policies fail before signing and preserve output", macos_only, async (context) => {
  const options = await fixture(context);
  await assert.rejects(packageApp(options, { CTLD_SIGNING_TIMESTAMP: "automatic" }), /CTLD_SIGNING_TIMESTAMP must be secure or none/);
  assert.deepEqual(await calls(options), []);
  await assertPreserved(options);
});
