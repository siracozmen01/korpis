// SPDX-License-Identifier: AGPL-3.0-or-later

// The panel's service worker (question 3 of §10 of docs/11-surface-web.md), served as `sw.js`
// beside the page: it keeps the files of the bundle it was built with, so a browser that loaded
// the panel once loads it again when the control plane cannot be reached, and it keeps nothing
// else (routing.ts). The build writes the bundle's files into it, so a new bundle is a new worker,
// which the browser installs at the next load, and which removes the old bundle's files.

/// <reference lib="webworker" />

import { route } from "./routing";

declare const self: ServiceWorkerGlobalScope;

/** The bundle's files, beneath the scope; written by the build. */
const SHELL: readonly string[] = ["__KORPIS_SHELL__"];
/** The cache that holds them, named for this bundle; written by the build. */
const CACHE = "korpis-shell-__KORPIS_VERSION__";
const PREFIX = "korpis-shell-";

const shell = new Set(SHELL);

self.addEventListener("install", (event) => {
  const scope = self.registration.scope;
  event.waitUntil(
    caches
      .open(CACHE)
      .then((cache) => cache.addAll(SHELL.map((path) => new URL(path, scope).href)))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((names) =>
        Promise.all(
          names.filter((n) => n.startsWith(PREFIX) && n !== CACHE).map((n) => caches.delete(n)),
        ),
      )
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const { request } = event;
  const scope = self.registration.scope;
  switch (route(request, scope, shell)) {
    case "shell":
      event.respondWith(caches.match(request).then((kept) => kept ?? fetch(request)));
      return;
    case "page":
      event.respondWith(
        fetch(request).catch(() =>
          caches.match(new URL("index.html", scope).href).then((kept) => kept ?? Response.error()),
        ),
      );
      return;
    case "network":
      return;
  }
});
