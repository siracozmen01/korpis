// SPDX-License-Identifier: AGPL-3.0-or-later

// A secret that arrives in the address's fragment (§6 of docs/11-surface-web.md): an invitation's
// or a share link's. A fragment is never sent to the server in a request line, so it lands in no
// access log and no `Referer`; the panel reads it once, keeps it in memory, and clears it from
// the address bar so it is not left in the history either.

/** The secrets taken so far, by prefix: a second render reads the one the first took. */
const held = new Map<string, string>();

/** The secret the fragment holds, if it starts with `prefix`; the address bar is cleared of it. */
export function takeFragment(prefix: string): string | undefined {
  const fragment = window.location.hash.slice(1);
  if (fragment.startsWith(prefix)) {
    held.set(prefix, fragment);
    const { pathname, search } = window.location;
    window.history.replaceState(null, "", pathname + search);
  }
  return held.get(prefix);
}

/** Forgets the secret held under `prefix`, once it has been used or refused. */
export function forgetFragment(prefix: string): void {
  held.delete(prefix);
}
