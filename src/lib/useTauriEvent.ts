import { useEffect, useRef } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";

/**
 * Subscribe to a Tauri event for the lifetime of the component.
 *
 * `listen()` resolves asynchronously, so a naive useEffect leaks the
 * subscription when React StrictMode mounts → unmounts → remounts in dev:
 * the first cleanup runs before the unlisten function exists, leaving two
 * live listeners (= duplicated events). This hook tracks disposal and
 * unsubscribes even when cleanup wins the race.
 *
 * `subscribe` must be referentially stable (module-level wrappers from
 * ipc.ts are). The callback is kept in a ref, so it can close over fresh
 * state without resubscribing.
 */
export function useTauriEvent<T>(
  subscribe: (cb: (e: T) => void) => Promise<UnlistenFn>,
  callback: (e: T) => void,
) {
  const cbRef = useRef(callback);
  cbRef.current = callback;

  useEffect(() => {
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    subscribe((e) => cbRef.current(e)).then((u) => {
      if (disposed) {
        u(); // cleanup already ran — undo the late subscription
      } else {
        unlisten = u;
      }
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [subscribe]);
}
