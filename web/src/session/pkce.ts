// SPDX-License-Identifier: AGPL-3.0-or-later

// PKCE (RFC 7636) for signing in through an identity provider (§9.7 of docs/08-identity.md): the
// panel makes the verifier and keeps it in the tab, and the control plane is given only its
// challenge, so a code taken from the redirect is worthless to anyone without the tab.

/** Base64url without padding, as RFC 7636 writes both values. */
function base64url(bytes: Uint8Array): string {
  let text = "";
  for (const b of bytes) text += String.fromCharCode(b);
  return btoa(text).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** A new verifier: 32 random bytes, 43 characters. */
export function verifier(): string {
  return base64url(crypto.getRandomValues(new Uint8Array(32)));
}

/** The `S256` challenge of `verifier`. */
export async function challenge(of: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(of));
  return base64url(new Uint8Array(digest));
}
