// SPDX-License-Identifier: AGPL-3.0-or-later

// A task's runs in the browser (§4 of docs/05-scheduling.md, §2 of docs/11-surface-web.md): its
// page lists them in place of a console, an active one is cancelled from the list, and, where a
// node runs tasks here, a run that ended by itself is opened and its own console read (§4 of
// docs/14-streams.md), from its node, after the run is gone from it.

import { expect, test, type Page } from "@playwright/test";
import { authenticator, given } from "./support";

/** Redeems the runner's invitation with a passkey and keeps the recovery codes. */
async function signedIn(page: Page): Promise<void> {
  await authenticator(page);
  await page.goto(`/invitation#${given("KORPIS_E2E_RUNNER_INVITATION")}`);
  await page.getByLabel("Choose a sign-in name").fill("runner");
  await page.getByRole("button", { name: "Register a passkey" }).click();
  await page.getByRole("button", { name: "I have kept them" }).click();
  await expect(page.getByRole("heading", { name: "What you can reach" })).toBeVisible();
}

/** Opens the task `name` of the project `games` from the start page. */
async function opened(page: Page, name: string): Promise<void> {
  await page.goto("/");
  await page.getByRole("link", { name: "acme", exact: true }).click();
  await page.getByRole("link", { name: "games", exact: true }).click();
  await page.getByRole("link", { name, exact: true }).click();
  await expect(page.getByRole("heading", { name, level: 1 })).toBeVisible();
}

test("a task's runs are listed, one is cancelled, and one's console is read", async ({ page }) => {
  test.setTimeout(120_000);
  given("KORPIS_E2E_WAITER");
  await signedIn(page);

  // Its first run was asked for as it was declared; it waits, pending or running, until ended.
  await opened(page, "waiter");
  const runs = page.getByRole("region", { name: "Runs" });
  await expect(
    page.getByRole("region", { name: "Console" }),
    "a task has no console of its own",
  ).toHaveCount(0);
  const run = runs.getByRole("row").filter({ hasText: "requested" });
  await expect(run).toHaveCount(1, { timeout: 15_000 });
  await expect(run).toContainText(/pending|running/);
  await run.getByRole("button", { name: "Cancel the run" }).click();
  await expect(run).toContainText("cancelled", { timeout: 15_000 });
  await expect(run.getByRole("button", { name: "Cancel the run" })).toHaveCount(0);

  if (process.env.KORPIS_E2E_PRINTER === undefined) return;
  // The printer's run prints three lines and exits zero; its status arrives as the node reports
  // it, and its console is still read once the run's object is gone.
  await opened(page, "printer");
  const printed = runs.getByRole("row").filter({ hasText: "requested" });
  await expect(printed).toContainText("succeeded", { timeout: 60_000 });
  await expect(printed).toContainText("it exited, status 0");
  await printed.getByRole("button", { name: "Console" }).click();
  const console_ = runs.getByRole("region", { name: "Console" });
  await expect(console_.getByText("backing up")).toBeVisible();
  await expect(console_.getByText("done", { exact: true })).toBeVisible();
});
