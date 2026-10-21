// SPDX-License-Identifier: AGPL-3.0-or-later

// Steps 3 to 6 of §44 of docs/23-walkthroughs.md, in a browser: Dana opens the sign-in link her
// company handed its staff, signs in through its identity provider, a real issuer, and reads who
// the panel says she is (§9.7 of docs/08-identity.md, §1 of docs/11-surface-web.md). Ten minutes
// on, she approves `korpis login` on a terminal, confirming it is her by signing in through the
// provider again, since she holds no passkey or password here.

import { expect, test } from "@playwright/test";
import { aged, given, visit } from "./support";

test("a person signs in through their provider, reads who they are, and approves a terminal", async ({
  page,
}) => {
  test.skip(process.env.KORPIS_E2E_SIGN_IN === undefined, "no issuer or node here");
  test.setTimeout(90_000);
  await visit(page, given("KORPIS_E2E_SIGN_IN"));
  await page.getByRole("button", { name: "Sign in with corp" }).click();
  await expect(page.getByRole("heading", { name: "What you can reach" })).toBeVisible();

  await visit(page, "/profile");
  await expect(page.getByRole("heading", { name: "dana", level: 1 })).toBeVisible();
  await expect(page.getByText(/^Signed in through corp/)).toBeVisible();

  await aged("KORPIS_E2E_AGE_PERSON");
  await visit(page, given("KORPIS_E2E_PERSON_DEVICE_URL"));
  await page.getByRole("button", { name: "Approve" }).click();
  await expect(page.getByRole("heading", { name: "Confirm it is you" })).toBeVisible();
  await page.getByRole("button", { name: "Sign in with corp again" }).click();
  await expect(page.getByRole("heading", { name: "Sign in a command-line client" })).toBeVisible();
  await page.getByRole("button", { name: "Approve" }).click();
  await expect(page.getByRole("heading", { name: "Signed in" })).toBeVisible();
});
