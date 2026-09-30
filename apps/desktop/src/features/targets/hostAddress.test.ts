import { describe, expect, it } from "vitest";
import { parseHostAddress } from "./hostAddress";

describe("host address", () => {
  it("parses structured user, host, port, and bracketed IPv6", () => {
    expect(parseHostAddress("rmux@127.0.0.1:2222")).toEqual({
      hostname: "127.0.0.1",
      user: "rmux",
      port: 2222,
    });
    expect(parseHostAddress("[::1]:2222")).toEqual({
      hostname: "::1",
      user: null,
      port: 2222,
    });
    expect(parseHostAddress("lab")).toEqual({
      hostname: "lab",
      user: null,
      port: null,
    });
  });
  it.each([
    "host:0",
    "host:65536",
    "ssh host -p 22",
    "-oProxyCommand=evil",
    "user@host;cmd",
    "host\ncommand",
  ])("rejects non-address input %s", (value) => {
    expect(parseHostAddress(value)).toBeNull();
  });
});
