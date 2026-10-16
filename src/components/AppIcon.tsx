import { useEffect, useState } from "react";
import { AppWindow } from "lucide-react";
import type { Backend } from "../lib/backend";

// Icons are extracted by Rust on demand and cached on disk. This module keeps
// a small in-memory map and batches requests made in the same tick.
const cache = new Map<string, string | null>();
const waiting = new Map<string, Array<(url: string | null) => void>>();
let scheduled = false;

function request(backend: Backend, key: string, resolve: (url: string | null) => void) {
  const list = waiting.get(key);
  if (list) {
    list.push(resolve);
    return;
  }
  waiting.set(key, [resolve]);
  if (scheduled) return;
  scheduled = true;
  queueMicrotask(async () => {
    scheduled = false;
    const keys = Array.from(waiting.keys());
    const callbacks = new Map(waiting);
    waiting.clear();
    let result: Record<string, string | null> = {};
    try {
      result = await backend.getIcons(keys);
    } catch {
      // Fall back to the generic icon; nothing to report to the user.
    }
    for (const k of keys) {
      // Missing keys were not resolved in time: show the fallback now and ask
      // again next time instead of remembering "no icon".
      const answered = Object.prototype.hasOwnProperty.call(result, k);
      const url = answered ? (result[k] ?? null) : null;
      if (answered) cache.set(k, url);
      callbacks.get(k)?.forEach((cb) => cb(url));
    }
  });
}

export function AppIcon({ backend, appKey, size = 32, running = true }: { backend: Backend; appKey: string; size?: number; running?: boolean }) {
  const [fetched, setFetched] = useState<{ key: string; url: string | null } | null>(null);
  const url = cache.has(appKey) ? cache.get(appKey) : fetched?.key === appKey ? fetched.url : undefined;
  useEffect(() => {
    if (cache.has(appKey)) return;
    let active = true;
    request(backend, appKey, (next) => {
      if (active) setFetched({ key: appKey, url: next });
    });
    return () => {
      active = false;
    };
  }, [backend, appKey]);

  return (
    <span className={`app-icon${running ? "" : " is-muted"}`} style={{ width: size, height: size }} aria-hidden>
      {url ? <img src={url} width={size} height={size} alt="" draggable={false} /> : <AppWindow size={Math.round(size * 0.6)} />}
    </span>
  );
}
