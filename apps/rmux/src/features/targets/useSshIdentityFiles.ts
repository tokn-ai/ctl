import { useEffect, useState } from "react";
import { errorMessage } from "../../lib/errors";
import { listSshIdentityFiles } from "../../lib/tauri";
import type { SshIdentityFileCatalog } from "../../lib/types";

interface IdentityFileDiscovery extends SshIdentityFileCatalog {
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
              `Could not list ~/.ssh: ${errorMessage(error)}. Enter a path manually.`,
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
