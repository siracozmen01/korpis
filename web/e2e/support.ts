// SPDX-License-Identifier: AGPL-3.0-or-later

// What the browser tests share: the secrets the harness passes in, and a passkey for the browser
// the way a person's phone holds one, through Chromium's virtual authenticator.

import type { Page } from "@playwright/test";

/** The environment variable `name`, which the harness sets. */
export function given(name: string): string {
  const value = process.env[name];
  if (value === undefined || value === "") throw new Error(`the harness sets ${name}`);
  return value;
}

/** Gives `page`'s browser a platform authenticator that holds discoverable passkeys and verifies
 * its user without asking, as a phone with a fingerprint reader would. */
export async function authenticator(page: Page): Promise<void> {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("WebAuthn.enable");
  await cdp.send("WebAuthn.addVirtualAuthenticator", {
    options: {
      protocol: "ctap2",
      transport: "internal",
      hasResidentKey: true,
      hasUserVerification: true,
      isUserVerified: true,
      automaticPresenceSimulation: true,
    },
  });
}
