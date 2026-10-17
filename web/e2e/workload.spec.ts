// SPDX-License-Identifier: AGPL-3.0-or-later

// A workload on a real node, used in a browser whose language is Turkish: an operator restarts it
// and is shown the plan after, types into its console and reads the answer, changes its
// configuration through the form its recipe generates, whose labels are the recipe's own Turkish
// ones (§5.3 of docs/09-recipes.md), previews a deletion and cancels it, uploads a file the
// network cuts off and resumes it, and shares it by a link whose uses run out. The panel itself
// has no Turkish catalogue yet, so its own words stay English.

import { type Browser, expect, test, type Page } from "@playwright/test";
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

/** The file uploaded: five mebibytes no two of whose chunks are alike, as the harness checks it
 * on the node, so a chunk out of place or missing cannot pass for the file. */
function world(): Buffer {
  const mib = 1 << 20;
  const bytes = Buffer.alloc(5 * mib);
  for (let i = 0; i < bytes.length; i += 1) bytes[i] = (i * 7 + Math.floor(i / mib)) % 251;
  return bytes;
}

/** An upload into the workload's volume that the network cuts off after its first chunks, and
 * that is resumed from what reached the node once the network is back (§8 of
 * docs/11-surface-web.md). */
async function uploadResumed(page: Page): Promise<void> {
  const files = page.getByRole("region", { name: "Files" });
  await files.getByRole("button", { name: "Open data" }).click();
  const file = { name: "world.bin", mimeType: "application/octet-stream", buffer: world() };
  // The stat and two chunks reach the relay; every request after them is lost, as on a train
  // going into a tunnel, its retries included.
  let sent = 0;
  await page.route("**/korpis.v1.RelayService/Send", (route) => {
    sent += 1;
    return sent <= 3 ? route.continue() : route.abort("internetdisconnected");
  });
  await files.getByLabel("Upload files").setInputFiles(file);
  await expect(files.getByText("a request could not be sent")).toBeVisible({ timeout: 15_000 });
  await expect(
    files.getByText(/^The upload of world\.bin stopped at 2 MiB of 5 MiB\.$/),
  ).toBeVisible();
  await page.unroute("**/korpis.v1.RelayService/Send");
  // The session that lost a request is not trusted again; another opens by itself.
  await expect(files.getByRole("button", { name: "Download world.bin" })).toBeVisible({
    timeout: 15_000,
  });
  await files.getByLabel("Upload files").setInputFiles(file);
  const asking = files.getByRole("group", { name: "Upload world.bin" });
  await expect(asking).toContainText("world.bin is there with 2");
  await asking.getByRole("button", { name: "Resume" }).click();
  await expect(files.getByText("Uploaded world.bin.")).toBeVisible({ timeout: 30_000 });
}

/** A share link that may only see the workload's status, twice, opened by someone with no
 * account in a browser of their own until its uses run out (§6 of docs/11-surface-web.md). */
async function sharedTwice(page: Page, browser: Browser): Promise<void> {
  const share = page.getByRole("region", { name: "Share as a link" });
  const console_ = share.getByRole("checkbox", { name: "read its console" });
  await share.getByText("read its console", { exact: true }).click();
  await expect(console_).not.toBeChecked();
  await share.getByLabel("Uses").fill("2");
  await share.getByRole("button", { name: "Make the link" }).click();
  const link = await page.getByRole("region", { name: "The link" }).locator("code").innerText();
  const elsewhere = await browser.newContext({ ignoreHTTPSErrors: true });
  const holder = await elsewhere.newPage();
  for (const use of [1, 2]) {
    await holder.goto("about:blank");
    await holder.goto(link);
    await expect(
      holder.getByRole("heading", { name: "talker", level: 1 }),
      `use ${use.toString()} shows the workload`,
    ).toBeVisible();
    await expect(holder.getByRole("button", { name: "Restart" })).toHaveCount(0);
  }
  await holder.goto("about:blank");
  await holder.goto(link);
  await expect(holder.getByRole("alert")).toHaveText(
    "This link has been used as many times as it allows, and opens nothing now.",
  );
  await expect(holder.getByRole("heading", { name: "talker", level: 1 })).toHaveCount(0);
  await elsewhere.close();
  await page.reload();
  const grants = page.getByRole("region", { name: "Who can act on it" });
  await expect(
    grants.getByRole("listitem").filter({ hasText: /^whoever holds a share link: .*used up/ }),
  ).toBeVisible();
}

test("a workload is restarted, typed into, configured, filled, and shared", async ({
  page,
  browser,
}) => {
  test.setTimeout(120_000);
  test.skip(
    process.env.KORPIS_E2E_WORKLOAD === undefined,
    "no node runs workloads here: the harness needs root beside containerd",
  );
  await signedIn(page);
  await page.getByRole("link", { name: "acme", exact: true }).click();
  await page.getByRole("link", { name: "games", exact: true }).click();
  await page.getByRole("link", { name: "talker", exact: true }).click();
  await expect(page.getByRole("heading", { name: "talker", level: 1 })).toBeVisible();

  const console_ = page.getByRole("region", { name: "Console" });
  await expect(console_.getByText("ready hello")).toBeVisible();

  // The recipe says a restart costs something: its plan is previewed in place and confirmed once
  // (§4 of docs/11-surface-web.md), and its convergence shown as it happens (step 5 of §38).
  await page.getByRole("button", { name: "Restart" }).click();
  const restarting = page.getByLabel("Preview");
  await expect(restarting.getByText("Restart will do this:")).toBeVisible();
  await expect(
    restarting.getByText("drops whoever is attached to its console").first(),
  ).toBeVisible();
  await restarting.getByRole("button", { name: "Confirm" }).click();
  await page.getByText("Restart: done. What it did").click();
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
  // The greeting is delivered at start, so the change restarts it, which costs something.
  await page.getByLabel("Preview").getByRole("button", { name: "Confirm" }).click();
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

  await uploadResumed(page);
  await sharedTwice(page, browser);
});
