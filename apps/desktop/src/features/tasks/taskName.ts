import type { TaskDefinition } from "../../lib/types";

const words = [
  "acorn", "badger", "birch", "breeze", "cedar", "clover", "coral", "dawn",
  "dune", "elm", "fern", "finch", "fox", "grove", "hazel", "heron",
  "ivy", "jade", "lark", "maple", "moss", "otter", "owl", "pearl",
  "pine", "reed", "robin", "sage", "spruce", "stone", "willow", "wren",
];

function namePart(path: string | null, fallback: string): string {
  const basename = path?.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? "";
  const normalized = basename.replace(/[\s\x00-\x1f]+/g, "-");
  let result = "";
  for (const character of normalized) {
    if (new TextEncoder().encode(result + character).length > 24) break;
    result += character;
  }
  return result || fallback;
}

export function generateTaskName(definition: TaskDefinition): string {
  const random = crypto.getRandomValues(new Uint32Array(1))[0];
  return [
    namePart(definition.program, "task"),
    namePart(definition.working_directory, "default"),
    words[random % words.length],
  ].join("-");
}
