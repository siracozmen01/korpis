// SPDX-License-Identifier: AGPL-3.0-or-later

// What the browser tests share: the secrets the harness passes in, and a passkey for the browser
// the way a person's phone holds one, through Chromium's virtual authenticator.

import { existsSync, writeFileSync } from "node:fs";
import type { Page } from "@playwright/test";

/** The environment variable `name`, which the harness sets. */
export function given(name: string): string {
  const value = process.env[name];
  if (value === undefined || value === "") throw new Error(`the harness sets ${name}`);
  return value;
}

/** An authenticator given to a browser, which can be taken away again. */
export interface Authenticator {
  readonly remove: () => Promise<void>;
}

/** Gives `page`'s browser a platform authenticator that holds discoverable passkeys and verifies
 * its user without asking, as a phone with a fingerprint reader would. */
export async function authenticator(page: Page): Promise<Authenticator> {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("WebAuthn.enable");
  const { authenticatorId } = await cdp.send("WebAuthn.addVirtualAuthenticator", {
    options: {
      protocol: "ctap2",
      transport: "internal",
      hasResidentKey: true,
      hasUserVerification: true,
      isUserVerified: true,
      automaticPresenceSimulation: true,
    },
  });
  return {
    remove: async () => {
      await cdp.send("WebAuthn.removeVirtualAuthenticator", { authenticatorId });
    },
  };
}

/** Has the harness age the keeper's browser sessions past the ten minutes that make one recent
 * (§9.4 of docs/08-identity.md): it is asked by a file appearing, and answers by removing it. */
export async function aged(): Promise<void> {
  const asked = given("KORPIS_E2E_AGE");
  writeFileSync(asked, "");
  for (let waited = 0; existsSync(asked); waited += 100) {
    if (waited > 10_000) throw new Error("the harness did not age the sessions");
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}
