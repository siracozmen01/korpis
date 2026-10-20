// SPDX-License-Identifier: AGPL-3.0-or-later

// A sign-in through an identity provider, begun in this tab (§9.7 of docs/08-identity.md, §1 of
// docs/11-surface-web.md): the verifier, where to return, and whether it re-authenticates the
// session rather than beginning one, kept in the tab's session storage while the person is at
// their provider. A tab that kept nothing, because another browser began the sign-in, completes
// nothing.

import type { Client } from "@connectrpc/connect";
import type { SessionService } from "../gen/korpis/v1/session_pb";
import { challenge, verifier } from "./pkce";

/** What the tab keeps while the person is away. */
export interface Pending {
  readonly verifier: string;
  readonly then: string;
  readonly reauthenticate: boolean;
}

/** The key the tab keeps it under. */
export const PENDING_KEY = "korpis.sign-in.external";

/** The tab's session storage, or a stand-in for it. */
export type Storage = Pick<globalThis.Storage, "getItem" | "setItem" | "removeItem">;

/** Takes what the tab kept, once: a second callback in the tab finds nothing. */
export function takePending(storage: Storage): Pending | undefined {
  const kept = storage.getItem(PENDING_KEY);
  storage.removeItem(PENDING_KEY);
  if (kept === null) return undefined;
  try {
    const p = JSON.parse(kept) as Partial<Pending>;
    if (typeof p.verifier !== "string" || typeof p.then !== "string") return undefined;
    return { verifier: p.verifier, then: p.then, reauthenticate: p.reauthenticate === true };
  } catch {
    return undefined;
  }
}

/** Where a provider is. */
export interface ProviderAt {
  readonly organization: string;
  readonly name: string;
}

/** Begins a sign-in through `provider` and answers where to send the browser, having kept what
 * the callback needs. */
export async function beginExternal(
  sessions: Client<typeof SessionService>,
  storage: Storage,
  provider: ProviderAt,
  after: { readonly then: string; readonly reauthenticate: boolean },
): Promise<string> {
  const v = verifier();
  const begun = await sessions.beginExternalSignIn({
    organization: provider.organization,
    provider: provider.name,
    codeChallenge: await challenge(v),
  });
  const pending: Pending = { verifier: v, ...after };
  storage.setItem(PENDING_KEY, JSON.stringify(pending));
  return begun.url;
}

/** `path` if it is one of this panel's own, and the panel's root otherwise: where a sign-in
 * returns to is never another site. */
export function ownPath(path: string): string {
  if (!path.startsWith("/") || path.startsWith("//") || path.includes("\\")) return "/";
  return path;
}
