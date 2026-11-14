import { useEffect, useState } from "react";
import { message, type DisplayText } from "../i18n/messages";
import { localizedErrorText } from "../../lib/errors";
import { listSshIdentityFiles } from "../../lib/tauri";
import type { SshIdentityFileCatalog } from "../../lib/types";

interface IdentityFileDiscovery extends Omit<SshIdentityFileCatalog, "warnings"> {
  warnings: DisplayText[];
  loading: boolean;
}

export function useSshIdentityFiles(enabled: boolean): IdentityFileDiscovery {
  const [discovery, setDiscovery] = useState<IdentityFileDiscovery>({
    identity_files: [],
    warnings: [],
    loading: true,
  });
  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    setDiscovery({ identity_files: [], warnings: [], loading: true });
    void listSshIdentityFiles().then(
      (catalog) => {
        if (!cancelled) setDiscovery({ ...catalog, loading: false });
      },
      (error: unknown) => {
        if (!cancelled)
          setDiscovery({
            identity_files: [],
            warnings: [
              message("ssh_flow.identity_discovery_failed", { detail: localizedErrorText(error) }),
            ],
            loading: false,
          });
      },
    );
    return () => {
      cancelled = true;
    };
  }, [enabled]);
  return discovery;
}
