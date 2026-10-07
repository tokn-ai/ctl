import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import { prefixActionMode, prefixActionKey, prefixEventKey, type resolvePrefix } from "./prefixKeymap";

type Mode = "prefix" | "move" | null;
interface Options {
  enabled: boolean;
  context: string;
  repeat_context?: string;
  keymap: ReturnType<typeof resolvePrefix>;
  on_action(action: string): void;
  on_input(data: Uint8Array): void;
}

export function useTerminalPrefix({ enabled, context, repeat_context = context, keymap, on_action, on_input }: Options) {
  const [mode, setMode] = useState<Mode>(null);
  const repeat_until = useRef(0);
  const repeat_action = useRef<string | null>(null);
  const repeatable = (action: string | undefined) => action?.startsWith("pane.resize_") || action?.startsWith("pane.focus_");
  const cancel = () => { repeat_until.current = 0; setMode(null); };
  const signature = JSON.stringify([...keymap.bindings]);
  useEffect(() => { repeat_until.current = 0; setMode(null); }, [enabled, repeat_context, keymap.label, signature]);
  useEffect(() => {
    setMode(null);
    if (!repeat_action.current?.startsWith("pane.focus_")) repeat_until.current = 0;
  }, [context]);
  useEffect(() => {
    const cancel = () => { repeat_until.current = 0; setMode(null); };
    window.addEventListener("blur", cancel);
    return () => window.removeEventListener("blur", cancel);
  }, []);
  function onKeyDown(event: KeyboardEvent) {
    const stroke = keymap.stroke;
    if (!enabled || !stroke) return;
    if (event.nativeEvent.isComposing || event.nativeEvent.getModifierState?.("AltGraph")) { cancel(); return; }
    if (!(event.target instanceof Element) || !event.target.closest(".terminal-container")) { cancel(); return; }
    const is_prefix = event.code === stroke.code && event.ctrlKey === stroke.ctrlKey && event.altKey === stroke.altKey && !event.metaKey && !event.shiftKey;
    const repeat = !mode && !is_prefix && Date.now() < repeat_until.current;
    if ((mode || repeat) && ["Shift", "Control", "Alt", "Meta"].includes(event.key)) {
      if (mode) { event.preventDefault(); event.stopPropagation(); }
      return;
    }
    const token = prefixEventKey(event);
    const action_mode = mode ?? "prefix";
    const action = token === null ? undefined : [...keymap.bindings].find(([id, key]) =>
      prefixActionMode(id) === action_mode && prefixActionKey(key) === token && (!repeat || repeatable(id)),
    )?.[0];
    if (!mode && !is_prefix && !(repeat && action)) { repeat_until.current = 0; return; }
    event.preventDefault();
    event.stopPropagation();
    if (event.key === "Escape" || event.key === "Enter" && mode === "move") { cancel(); return; }
    if (is_prefix) {
      if (event.repeat) return;
      repeat_until.current = 0;
      if (mode) { setMode(null); on_input(stroke.bytes); }
      else setMode("prefix");
      return;
    }
    if (event.repeat && !repeatable(action)) return;
    if (mode !== "move") setMode(null);
    repeat_action.current = action ?? null;
    repeat_until.current = repeatable(action) ? Date.now() + 500 : 0;
    if (action === "pane.move") setMode("move");
    else if (action) on_action(action);
  }
  return { mode, onKeyDown, cancel };
}
