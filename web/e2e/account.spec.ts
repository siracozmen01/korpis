// SPDX-License-Identifier: AGPL-3.0-or-later

// Steps 2 and 3 of §38 of docs/23-walkthroughs.md, in a browser: the friend redeems the invitation
// the operator issued by registering a passkey, keeps the recovery codes, signs out, signs in
// again with the passkey alone, and approves `korpis login` on a terminal.

import { expect, test } from "@playwright/test";
import { authenticator, given, visit } from "./support";

test("an invitation is redeemed with a passkey, which signs its holder in and a terminal too", async ({
  page,
}) => {
  await authenticator(page);
  await visit(page, `/invitation#${given("KORPIS_E2E_INVITATION")}`);
  await expect(page.getByRole("heading", { name: "Welcome, Friend" })).toBeVisible();
  expect(page.url(), "the invitation is cleared from the address").not.toContain("korpisiv_");
  await page.getByLabel("Choose a sign-in name").fill("friend");
  await page.getByRole("button", { name: "Register a passkey" }).click();
  await expect(page.getByRole("heading", { name: "Your recovery codes" })).toBeVisible();
  await expect(page.locator("ol code")).toHaveCount(10);
  await page.getByRole("button", { name: "I have kept them" }).click();
  await expect(page.getByRole("heading", { name: "What you can reach" })).toBeVisible();

  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(page).toHaveURL(/\/sign-in$/);
  await page.getByRole("button", { name: "Sign in with a passkey" }).click();
  await expect(page.getByRole("heading", { name: "What you can reach" })).toBeVisible();

  // `korpis login` waits in the harness with a code; the friend, just signed in, approves it at
  // the address it named, having been shown what asks and from where (§9.4 of
  // docs/08-identity.md).
  await visit(page, given("KORPIS_E2E_DEVICE_URL"));
  await expect(page.getByRole("heading", { name: "Sign in a command-line client" })).toBeVisible();
  await expect(page.getByText("It asked from")).toBeVisible();
  await page.getByRole("button", { name: "Approve" }).click();
  await expect(page.getByRole("heading", { name: "Signed in" })).toBeVisible();
});
