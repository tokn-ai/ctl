import { describe, expect, it } from "vitest";
import { generateTaskName } from "./taskName";

function name(program: string, working_directory: string | null) {
  return generateTaskName({ name: "", program, working_directory, arguments: [], execution_mode: "background" });
}

describe("generated task names", () => {
  it("uses executable and folder basenames on both platforms", () => {
    expect(name("/usr/bin/cargo", "/projects/my-project/")).toMatch(/^cargo-my-project-[a-z]+$/);
    expect(name("C:\\Tools\\node.exe", "C:\\Projects\\my app\\")).toMatch(/^node.exe-my-app-[a-z]+$/);
  });

  it("handles missing and root directories", () => {
    expect(name("cargo", null)).toMatch(/^cargo-default-[a-z]+$/);
    expect(name("cargo", "/")).toMatch(/^cargo-default-[a-z]+$/);
  });

  it("keeps Unicode names within the 64-byte limit", () => {
    const generated = name("界".repeat(40), "/" + "界".repeat(40));
    expect(new TextEncoder().encode(generated).length).toBeLessThanOrEqual(64);
    expect(generated).not.toContain("�");
  });
});
