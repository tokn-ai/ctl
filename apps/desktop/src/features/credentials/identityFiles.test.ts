import { describe, expect, it } from "vitest";
import { identityMutationError } from "./identityFiles";

describe("Identity mutation errors", () => {
  it.each(["__proto__", "constructor", "toString", "sample-secret-fixture", { code: "unexpected", message: "sample-secret-fixture" }, { code: { value: "sample-secret-fixture" } }])("uses a static fallback for an unknown error %j", (failure) => {
    expect(identityMutationError(failure)).toBe("The passphrase could not be saved. Check the passphrase, identity file, and Keychain access, then try again.");
  });

  it("accepts an exact code string and keeps forget failures specific to the action", () => {
    expect(identityMutationError("identity_unlock_failed")).toBe("The passphrase does not unlock this identity file. Try again.");
    expect(identityMutationError(new Error("sample-secret-fixture"), "forget")).toBe("The saved passphrase could not be forgotten. Check Keychain access and try again.");
  });

  it.each(["credential_store_busy", "identity_keychain_busy"])("explains %s without rendering native details", (code) => {
    for (const action of ["save", "forget"] as const) {
      expect(identityMutationError({ code, message: "sample-secret-fixture" }, action))
        .toBe("Another Keychain request is still active. Complete or cancel it, then try again.");
    }
  });
});
