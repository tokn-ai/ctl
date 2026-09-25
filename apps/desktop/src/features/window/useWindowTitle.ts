import { useEffect } from "react";
import { setNativeWindowTitle } from "../../lib/tauri";

export interface WindowTitleTarget {
  setDocumentTitle(title: string): void;
  setNativeTitle(title: string): Promise<void>;
}

/**
 * Serializes native title writes and always lets the newest requested title
 * finish last. Native title calls are asynchronous, so this prevents an older
 * attachment snapshot from overwriting a newer tab after it resolves late.
 */
export class WindowTitleWriter {
  private requestedTitle: string | null = null;
  private appliedNativeTitle: string | null = null;
  private writing = false;
  private disposed = false;

  constructor(private readonly target: WindowTitleTarget) {}

  setTitle(title: string): void {
    if (this.disposed || title === this.requestedTitle) {
      return;
    }

    this.requestedTitle = title;
    this.target.setDocumentTitle(title);
    void this.flush();
  }

  dispose(): void {
    this.disposed = true;
    this.requestedTitle = null;
  }

  private async flush(): Promise<void> {
    if (this.writing || this.disposed) {
      return;
    }

    const title = this.requestedTitle;
    if (title === null || title === this.appliedNativeTitle) {
      return;
    }

    this.writing = true;
    try {
      await this.target.setNativeTitle(title);
      if (!this.disposed && this.requestedTitle === title) {
        this.appliedNativeTitle = title;
      }
    } catch {
      // The browser title still updates when running outside Tauri or while a
      // native window is shutting down. A later title change retries normally.
    } finally {
      this.writing = false;
      if (!this.disposed && this.requestedTitle !== title) {
        void this.flush();
      }
    }
  }
}

const currentWindowTitleTarget: WindowTitleTarget = {
  setDocumentTitle(title) {
    document.title = title;
  },
  async setNativeTitle(title) {
    await setNativeWindowTitle(title);
  },
};

// Every Tauri WebView has its own JavaScript module graph, making this writer
// window-local while also keeping Strict Mode and remounts on one native queue.
const currentWindowTitleWriter = new WindowTitleWriter(currentWindowTitleTarget);

/** Updates this WebView's document title and its owning native window title. */
export function useWindowTitle(title: string): void {
  useEffect(() => {
    currentWindowTitleWriter.setTitle(title);
  }, [title]);
}
