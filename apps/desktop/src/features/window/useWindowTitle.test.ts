import { describe, expect, it, vi } from "vitest";
import {
  type WindowTitleTarget,
  WindowTitleWriter,
} from "./useWindowTitle";

interface Deferred {
  promise: Promise<void>;
  resolve(): void;
}

function deferred(): Deferred {
  let resolve!: () => void;
  const promise = new Promise<void>((finish) => {
    resolve = finish;
  });
  return { promise, resolve };
}

async function flushMicrotasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

describe("WindowTitleWriter", () => {
  it("serializes native writes so a stale title cannot finish last", async () => {
    const first = deferred();
    const second = deferred();
    const documentTitles: string[] = [];
    const nativeTitles: string[] = [];
    const target: WindowTitleTarget = {
      setDocumentTitle: (title) => documentTitles.push(title),
      setNativeTitle: vi.fn((title) => {
        nativeTitles.push(title);
        return nativeTitles.length === 1 ? first.promise : second.promise;
      }),
    };
    const writer = new WindowTitleWriter(target);

    writer.setTitle("/one — zsh");
    writer.setTitle("/two — cargo test");

    expect(documentTitles).toEqual(["/one — zsh", "/two — cargo test"]);
    expect(nativeTitles).toEqual(["/one — zsh"]);

    first.resolve();
    await flushMicrotasks();
    expect(nativeTitles).toEqual(["/one — zsh", "/two — cargo test"]);

    second.resolve();
    await flushMicrotasks();
  });

  it("does not enqueue a newer native write after disposal", async () => {
    const first = deferred();
    const nativeTitles: string[] = [];
    const writer = new WindowTitleWriter({
      setDocumentTitle: vi.fn(),
      setNativeTitle: (title) => {
        nativeTitles.push(title);
        return first.promise;
      },
    });

    writer.setTitle("/one — zsh");
    writer.setTitle("/two — cargo test");
    writer.dispose();
    first.resolve();
    await flushMicrotasks();

    expect(nativeTitles).toEqual(["/one — zsh"]);
  });
});
