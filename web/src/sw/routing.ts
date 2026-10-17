// SPDX-License-Identifier: AGPL-3.0-or-later

// What the panel's service worker does with each request (question 3 of §10 of
// docs/11-surface-web.md): the bundle may be cached, what it shows may not. A file of the bundle,
// named by its content's hash, is answered from the cache; a page is asked of the network first,
// so a reachable server always gives the current bundle, and answered with the cached panel only
// when the network cannot answer; everything else, every call of the API, a relay, the source
// notice, is left to the network and never kept.

/** What the worker does with a request. */
export type Route = "shell" | "page" | "network";

/** The parts of a request the route depends on. */
export interface Asked {
  readonly method: string;
  readonly url: string;
  readonly mode: string;
}

/** The route of `asked`, made from `scope`, whose bundle's files are `shell` (paths beneath it). */
export function route(asked: Asked, scope: string, shell: ReadonlySet<string>): Route {
  if (asked.method !== "GET") return "network";
  const url = new URL(asked.url);
  const base = new URL(scope);
  if (url.origin !== base.origin || !url.pathname.startsWith(base.pathname)) return "network";
  const path = url.pathname.slice(base.pathname.length);
  if (path.startsWith("about/")) return "network";
  if (shell.has(path)) return "shell";
  return asked.mode === "navigate" ? "page" : "network";
}
