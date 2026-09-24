import { useEffect, useState } from "react";
import type { KeyboardEvent } from "react";
import { prefixActionMode, prefixActionKey, type resolvePrefix } from "./prefixKeymap";

type Mode = "prefix" | "move" | null;
interface Options {
  enabled: boolean;
  context: string;
  keymap: ReturnType<typeof resolvePrefix>;
  on_action(action: string): void;
  on_input(data: Uint8Array): void;
}

export function useTerminalPrefix({ enabled, context, keymap, on_action, on_input }: Options) {
  const [mode, setMode] = useState<Mode>(null);
  const signature = JSON.stringify([...keymap.bindings]);
  useEffect(() => { setMode(null); }, [enabled, context, keymap.label, signature]);
  useEffect(() => {
    const cancel = () => setMode(null);
    window.addEventListener("blur", cancel);
    return () => window.removeEventListener("blur", cancel);
  }, []);
  function onKeyDown(event: KeyboardEvent) {
    const stroke = keymap.stroke;
    if (!enabled || !stroke) return;
    if (event.nativeEvent.isComposing || event.nativeEvent.getModifierState?.("AltGraph")) { setMode(null); return; }
    if (!(event.target instanceof Element) || !event.target.closest(".terminal-container")) return;
    const is_prefix = event.code === stroke.code && event.ctrlKey === stroke.ctrlKey && event.altKey === stroke.altKey && !event.metaKey && !event.shiftKey;
    if (!mode && !is_prefix) return;
    event.preventDefault();
    event.stopPropagation();
    if (event.repeat) return;
    if (event.key === "Escape" || event.key === "Enter" && mode === "move") { setMode(null); return; }
    if (is_prefix) {
      if (mode) { setMode(null); on_input(stroke.bytes); }
      else setMode("prefix");
      return;
    }
    if (["Shift", "Control", "Alt", "Meta"].includes(event.key)) return;
    if (mode !== "move") setMode(null);
    if (event.ctrlKey || event.altKey || event.metaKey) return;
    const action = [...keymap.bindings].find(([id, key]) => prefixActionMode(id) === mode && prefixActionKey(key) === (event.key.length === 1 ? event.key.toLowerCase() : event.key))?.[0];
    if (action === "pane.move") setMode("move");
    else if (action) on_action(action);
  }
  return { mode, onKeyDown, cancel: () => setMode(null) };
}
