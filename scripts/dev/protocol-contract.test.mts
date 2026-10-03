import assert from "node:assert/strict";
import test from "node:test";
import {
  negotiateProtocol, parseHelperComponent, parseHelperProtocols, parseProtocolInfo, sameProtocols,
  type ProtocolInfo,
} from "../shared/protocol-contract.mts";

const older: ProtocolInfo = { name: "ctld", build: 13, version: "1.0.13", supported_versions: ["1.0.13"] };
const newer: ProtocolInfo = { name: "ctld", build: 16, version: "1.1.15", supported_versions: ["1.0.13", "1.1.15"] };
const auxiliary = ["ctld_lifecycle", "ctld_helper"].map((name) => ({ name, build: 1, version: "1.0.1", supported_versions: ["1.0.1"] }));
const build = { version: "0.1.0", source_revision: null, source_fingerprint: "a".repeat(64), dirty: false };

test("published contracts negotiate symmetrically with skipped internal builds", () => {
  assert.equal(negotiateProtocol(older, newer), "1.0.13");
  assert.equal(negotiateProtocol(newer, older), "1.0.13");
  assert.equal(negotiateProtocol(newer, newer), "1.1.15");
  assert.equal(negotiateProtocol(newer, { ...older, build: 14, version: "1.0.14", supported_versions: ["1.0.14"] }), undefined);
  assert.equal(negotiateProtocol(newer, { ...older, version: "2.0.13", supported_versions: ["2.0.13"] }), undefined);
  assert.equal(negotiateProtocol(newer, { ...older, name: "other" }), undefined);
});

test("metadata rejects noncanonical triplets and contradictory support claims", () => {
  for (const version of [12, "0.0.13", "01.0.13", "1.00.13", "1.0.013", "1.0", "1.0.13-dev", "1.0.13+build", "^1.0.13", "1.65536.13", "1.0.13\n"]) {
    assert.throws(() => parseProtocolInfo({ ...older, version, supported_versions: [version] }));
  }
  for (const changed of [
    { build: 14 }, { supported_versions: [] }, { supported_versions: ["1.0.13", "1.0.13", "1.1.15"] },
    { supported_versions: ["2.0.13", "1.1.15"] }, { supported_versions: ["1.0.16", "1.1.15"] },
    { supported_versions: ["1.0.15", "1.1.15"] }, { supported_versions: ["1.1.15", "1.2.16"] },
    { supported_versions: Array.from({ length: 129 }, (_, index) => `1.0.${index}`) }, { name: "bad name" }, { trust_override: true },
  ]) assert.throws(() => parseProtocolInfo({ ...newer, ...changed }));
});

test("helper metadata requires every uniquely named protocol and bounded source identity", () => {
  const protocols = [newer, ...auxiliary];
  assert.deepEqual(parseHelperComponent(JSON.stringify({ build, protocols })).protocols, protocols);
  assert.throws(() => parseHelperProtocols([]));
  assert.throws(() => parseHelperProtocols([newer, newer, ...auxiliary]));
  assert.throws(() => parseHelperComponent(JSON.stringify({ build: { ...build, dirty: "false" }, protocols })));
  assert.throws(() => parseHelperComponent(JSON.stringify({ build, protocols, extra: "a".repeat(16 * 1024) })), /oversized/);
  assert.equal(sameProtocols(protocols, [{ ...newer, supported_versions: [...newer.supported_versions].reverse() }, ...auxiliary].reverse()), true);
});
