// SPDX-License-Identifier: AGPL-3.0-or-later

// A workload on a real node, used in a browser whose language is Turkish: an operator restarts it
// and is shown the plan after, types into its console and reads the answer, changes its
// configuration through the form its recipe generates, whose labels are the recipe's own Turkish
// ones (§5.3 of docs/09-recipes.md), and previews a deletion and cancels it. The panel itself has
// no Turkish catalogue yet, so its own words stay English.

import { expect, test, type Page } from "@playwright/test";
import { authenticator, given } from "./support";

test.use({ locale: "tr-TR" });

/** Redeems the operator's invitation with a passkey and keeps the recovery codes. */
async function signedIn(page: Page): Promise<void> {
  await authenticator(page);
  await page.goto(`/invitation#${given("KORPIS_E2E_OPERATOR_INVITATION")}`);
  await page.getByLabel("Choose a sign-in name").fill("operator");
  await page.getByRole("button", { name: "Register a passkey" }).click();
  await page.getByRole("button", { name: "I have kept them" }).click();
  await expect(page.getByRole("heading", { name: "What you can reach" })).toBeVisible();
}

test("a workload is restarted, typed into, and configured", async ({ page }) => {
  test.skip(
    process.env.KORPIS_E2E_WORKLOAD === undefined,
    "no node runs workloads here: the harness needs root beside containerd",
  );
  await signedIn(page);
  await page.getByRole("link", { name: "acme" }).click();
  await page.getByRole("link", { name: "games" }).click();
  await page.getByRole("link", { name: "talker" }).click();
  await expect(page.getByRole("heading", { name: "talker", level: 1 })).toBeVisible();

  const console_ = page.getByRole("region", { name: "Console" });
  await expect(console_.getByText("ready hello")).toBeVisible();

  // A restart is reversible, loses nothing, and touches one workload: applied at once, its plan
  // shown after rather than a confirmation asked for (§4 of docs/11-surface-web.md).
  await page.getByRole("button", { name: "Restart" }).click();
  await page.getByText("Restart: done. What it did").click();
  await expect(page.getByRole("region", { name: "Actions" }).getByRole("list")).toBeVisible();
  await expect(page.getByRole("row", { name: /^Converged \+/ })).toBeVisible({ timeout: 30_000 });
  await expect(console_.getByText("ready hello")).toHaveCount(2, { timeout: 30_000 });

  await console_.getByLabel("Send a command").fill("ping");
  await console_.getByRole("button", { name: "Send" }).click();
  // What was typed is shown as who typed it, and the workload's answer as it wrote it.
  await expect(console_.getByText(/^user:\S+ typed: ping$/)).toBeVisible();
  await expect(console_.getByText("got ping")).toBeVisible();

  const configuration = page.getByRole("region", { name: "Configuration" });
  await expect(configuration.getByText("Cevap veren konsol")).toBeVisible();
  await configuration.getByLabel("Selamlama").fill("merhaba");
  await configuration.getByRole("button", { name: "Save the configuration" }).click();
  await expect(page.getByText("Configuration change: done. What it did")).toBeVisible();
  await expect(console_.getByText("ready merhaba")).toBeVisible({ timeout: 30_000 });

  // Deleting interrupts service; the talker holds no data, so nothing is at risk: its plan is
  // previewed in place with one confirmation. Cancelled here, and the workload stays.
  await page.getByRole("button", { name: "Delete" }).click();
  const preview = page.getByLabel("Preview");
  await expect(preview.getByText("Delete will do this:")).toBeVisible();
  await expect(preview.getByText("(interrupts service)")).toBeVisible();
  await preview.getByRole("button", { name: "Cancel" }).click();
  await expect(page.getByRole("button", { name: "Delete" })).toBeEnabled();
  await expect(page.getByText("Declared running, converged")).toBeVisible();
});
