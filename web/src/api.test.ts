// SPDX-License-Identifier: AGPL-3.0-or-later

import { controlPlane, sessions } from "./api";

test("the panel calls the control plane it was served by, through the generated client", () => {
  expect(controlPlane()).toBe(window.location.origin);
  expect(typeof sessions.exchange).toBe("function");
});
