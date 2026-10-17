// SPDX-License-Identifier: AGPL-3.0-or-later

// Registers the panel's service worker (worker.ts), in a built panel only: in development the
// files change under it, and a cached copy would only hide the change. A browser that refuses it
// loads the panel from the network every time, which is all a worker's absence costs.

/** Registers `sw.js` for the panel's own path. */
export function register(): void {
  if (!import.meta.env.PROD || !("serviceWorker" in navigator)) return;
  const base = import.meta.env.BASE_URL;
  navigator.serviceWorker.register(`${base}sw.js`, { scope: base }).catch(() => undefined);
}
