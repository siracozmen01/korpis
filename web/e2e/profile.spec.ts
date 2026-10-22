// SPDX-License-Identifier: AGPL-3.0-or-later

// Steps 11 and 14 of §38 of docs/23-walkthroughs.md, in a browser: the keeper of an organization
// dresses it in a theme that everyone whose organization it is then wears (§7 of
// docs/11-surface-web.md); confirms it is them once their sign-in is more than ten minutes old
// and adds a second passkey on another device, which then signs them in alone (§9.6 of
// docs/08-identity.md); and loads the panel again with no network, from the browser's copy of
// it, which says the control plane cannot be reached and when it was last heard from. The theme
// it set is in the organization's audit trail (§4 of docs/15-observability.md).

import { expect, test } from "@playwright/test";
import { aged, authenticator, given, visit } from "./support";

test("a keeper themes the organization, adds a passkey, and loads the panel offline", async ({
  page,
  context,
}) => {
  test.setTimeout(90_000);
  const phone = await authenticator(page);
  await visit(page, `/invitation#${given("KORPIS_E2E_KEEPER_INVITATION")}`);
  await page.getByLabel("Choose a sign-in name").fill("keeper");
  await page.getByRole("button", { name: "Register a passkey" }).click();
  await page.getByRole("button", { name: "I have kept them" }).click();
  await expect(page.getByRole("heading", { name: "What you can reach" })).toBeVisible();

  // The theme, saved on the organization, and worn by its own people from the next load on.
  await page.getByRole("link", { name: "acme", exact: true }).click();
  const theme = page.getByRole("region", { name: "Theme" });
  await theme.getByLabel("Product name").fill("Acme Games");
  await theme
    .getByRole("group", { name: "Light colours" })
    .getByLabel("Accent (#rrggbb)", { exact: true })
    .fill("#5a189a");
  await theme.getByRole("button", { name: "Save" }).click();
  await expect(theme.getByText("Saved.")).toBeVisible();
  // The organization's audit trail records it as the keeper's, read under their org.read; the
  // trail is its whole subtree's, so the operator's theme for a reseller beneath it is there too.
  await visit(page, page.url());
  const audit = page.getByRole("region", { name: "Audit" });
  const themed = audit.getByRole("listitem").filter({ hasText: "org.theme.set" });
  await expect(themed.filter({ hasText: "by user " })).toBeVisible();
  await visit(page, "/");
  await expect(page).toHaveTitle("Acme Games");
  const accent = await page.evaluate(() =>
    getComputedStyle(document.documentElement).getPropertyValue("--accent").trim(),
  );
  expect(accent).toBe("#5a189a");

  // Ten minutes on, a change to how they sign in asks them to confirm it is them first.
  await aged();
  await visit(page, "/profile");
  await page.getByRole("button", { name: "Make new ones" }).click();
  await expect(page.getByRole("heading", { name: "Confirm it is you" })).toBeVisible();
  await page.getByRole("button", { name: "Use a passkey" }).click();
  await expect(page.getByRole("heading", { name: "Your recovery codes" })).toBeVisible();
  await expect(page.locator("ol code")).toHaveCount(10);
  await page.getByRole("button", { name: "I have kept them" }).click();

  // A second passkey, on a security key, with the phone put away; it signs them in alone.
  await phone.remove();
  await authenticator(page);
  await page.getByLabel("Name for a new passkey").fill("security key");
  await page.getByRole("button", { name: "Add a passkey" }).click();
  await expect(page.getByText(/^security key, added/)).toBeVisible();
  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(page).toHaveURL(/\/sign-in/);
  await page.getByRole("button", { name: "Sign in with a passkey" }).click();
  await expect(page.getByRole("heading", { name: "Keeper", level: 1 })).toBeVisible();

  // With no network, the panel loads from the browser's copy of it and says what it cannot know.
  await page.evaluate(() => navigator.serviceWorker.ready.then(() => undefined));
  await context.setOffline(true);
  await page.reload();
  await expect(page.getByRole("alert")).toHaveText("The control plane cannot be reached.");
  await expect(page.getByText(/^This browser last heard from it .+ ago\.$/)).toBeVisible();
  await context.setOffline(false);
  await page.getByRole("button", { name: "Try again" }).click();
  await expect(page.getByRole("heading", { name: "Keeper", level: 1 })).toBeVisible();
});
