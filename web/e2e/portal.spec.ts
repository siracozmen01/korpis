// SPDX-License-Identifier: AGPL-3.0-or-later

// Trace 48 of docs/23-walkthroughs.md, in a browser: Ada redeems an invitation at Pinecone's
// portal, a name of its own the control plane answers under (§3.6 of docs/07-networking.md),
// registering a passkey for the portal's own relying party; the panel wears Pinecone's theme
// there and lands on the landing screen Pinecone composed, though her grant lies above Pinecone;
// her profile there shows what was begun there alone; and setting a password there is refused,
// since her grant reaches beyond Pinecone (§9.8 of docs/08-identity.md).

import { expect, test } from "@playwright/test";
import { authenticator, given, visit } from "./support";

test("a reseller's customer signs in at the reseller's portal, confined beneath it", async ({
  page,
}) => {
  const portal = given("KORPIS_E2E_PORTAL_URL");
  await authenticator(page);
  await visit(page, `${portal}/invitation#${given("KORPIS_E2E_PORTAL_INVITATION")}`);
  await expect(page.getByRole("heading", { name: "Welcome, Ada" })).toBeVisible();
  await page.getByLabel("Choose a sign-in name").fill("ada");
  await page.getByRole("button", { name: "Register a passkey" }).click();
  await expect(page.getByRole("heading", { name: "Your recovery codes" })).toBeVisible();
  await page.getByRole("button", { name: "I have kept them" }).click();
  await expect(page.getByRole("heading", { name: "What you can reach" })).toBeVisible();
  await expect(page, "the portal's organization's theme, not her provider's").toHaveTitle(
    "Pinecone Panel",
  );
  // And the landing screen Pinecone composed, its link shown with where it goes.
  await expect(page.getByRole("heading", { name: "Welcome to Pinecone" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Pinecone support" })).toBeVisible();
  await expect(page.getByText("(goes to help.pinecone.localhost)")).toBeVisible();

  // The passkey signs her in there again.
  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(page).toHaveURL(/\/sign-in$/);
  await page.getByRole("button", { name: "Sign in with a passkey" }).click();
  await expect(page.getByRole("heading", { name: "What you can reach" })).toBeVisible();

  // Her profile there holds what was begun there: this session, and the passkey registered.
  await visit(page, `${portal}/profile`);
  const sessions = page.getByRole("region", { name: "Where you are signed in" });
  await expect(sessions.getByText("(this one)")).toBeVisible();
  await expect(sessions.getByRole("button", { name: "End it" })).toHaveCount(0);

  // A password serves every address, and her grant reaches beyond Pinecone: it is set elsewhere.
  await page.getByLabel("A password, at least 12 characters").fill("correct horse battery");
  await page.getByRole("button", { name: "Set a password" }).click();
  await expect(page.getByRole("alert")).toContainText("reaches beyond this organization");
});
