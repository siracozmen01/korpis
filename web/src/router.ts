// SPDX-License-Identifier: AGPL-3.0-or-later

// The panel's routes: the address bar's path, read and changed through the history API. A router
// library would bring more than the panel uses; this is the part it uses.

import { useEffect, useSyncExternalStore } from "react";

/** The event a change made here announces, since `pushState` announces nothing itself. */
const CHANGED = "korpis:navigate";

function subscribe(changed: () => void): () => void {
  window.addEventListener("popstate", changed);
  window.addEventListener(CHANGED, changed);
  return () => {
    window.removeEventListener("popstate", changed);
    window.removeEventListener(CHANGED, changed);
  };
}

/** The path the panel is at, rendered again whenever it changes. */
export function usePath(): string {
  return useSyncExternalStore(subscribe, () => window.location.pathname);
}

/** Goes to `path`, as a new entry of the history or in place of the current one. */
export function navigate(path: string, options: { replace?: boolean } = {}): void {
  if (options.replace === true) window.history.replaceState(null, "", path);
  else window.history.pushState(null, "", path);
  window.dispatchEvent(new Event(CHANGED));
}

/** Goes to `to` once rendered, in place of the current entry: a route that is not allowed. */
export function Redirect({ to }: { readonly to: string }) {
  useEffect(() => {
    navigate(to, { replace: true });
  }, [to]);
  return null;
}
