// SPDX-License-Identifier: AGPL-3.0-or-later

// The panel's browser tests (§9 of docs/21-stack.md): run by the browser test of
// korpis-e2e against a real control plane serving the built panel, whose address and the secrets
// the tests begin from it passes in the environment. One browser, one test at a time: each test
// is a step of one person's use of one installation, as §38 of docs/23-walkthroughs.md traces it.

import { defineConfig, devices } from "@playwright/test";

const url = process.env.KORPIS_E2E_URL;
if (url === undefined) throw new Error("KORPIS_E2E_URL names the control plane serving the panel");

export default defineConfig({
  testDir: ".",
  testMatch: "*.spec.ts",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 60_000,
  reporter: [["list"]],
  use: {
    ...devices["Desktop Chrome"],
    baseURL: url,
    // The control plane's own CA signed its certificate; the browser is not told of it. A page
    // is let past that by `ignoreHTTPSErrors`, but Chromium registers no service worker from an
    // origin whose certificate it did not trust unless told to ignore the error itself.
    ignoreHTTPSErrors: true,
    launchOptions: { args: ["--ignore-certificate-errors"] },
    trace: "retain-on-failure",
  },
});
