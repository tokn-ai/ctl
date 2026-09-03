import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { loadKeybindings, saveKeybindings } from "../../lib/tauri";
import type { KeybindingsDocument, KeybindingsSnapshot } from "../../lib/types";
import { errorMessage } from "../../lib/errors";
import { EMPTY_KEYBINDINGS, resolveKeymap } from "./keymap";
import type { ShortcutPlatform } from "./types";

export function useKeybindings(platform: ShortcutPlatform) {
  const [snapshot, setSnapshot] = useState<KeybindingsSnapshot>({
    path: "",
    revision: null,
    document: EMPTY_KEYBINDINGS,
  });
  const [error, setError] = useState<string | null>(null);
  const [ready, setReady] = useState(false);
  const latest = useRef(snapshot);
  const busy = useRef(false);
  const epoch = useRef(0);
  const reload = useCallback(async () => {
    const token = ++epoch.current;
    try {
      const loaded = await loadKeybindings();
      resolveKeymap(loaded.document, platform);
      if (token !== epoch.current) return;
      latest.current = loaded;
      setSnapshot(loaded);
      setError(null);
    } catch (failure) {
      if (token === epoch.current) setError(errorMessage(failure));
    } finally {
      if (token === epoch.current) setReady(true);
    }
  }, [platform]);
  useEffect(() => {
    void reload();
    return () => {
      epoch.current += 1;
    };
  }, [reload]);
  const save = useCallback(
    async (document: KeybindingsDocument) => {
      if (busy.current)
        throw new Error("Keyboard shortcuts are already being saved.");
      resolveKeymap(document, platform);
      busy.current = true;
      const token = ++epoch.current;
      try {
        const saved = await saveKeybindings(latest.current.revision, document);
        if (token === epoch.current) {
          latest.current = saved;
          setSnapshot(saved);
          setError(null);
        }
      } finally {
        busy.current = false;
      }
    },
    [platform],
  );
  const bindings = useMemo(
    () => resolveKeymap(snapshot.document, platform),
    [snapshot.document, platform],
  );
  return { ...snapshot, bindings, ready, error, reload, save };
}
