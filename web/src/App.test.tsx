// SPDX-License-Identifier: AGPL-3.0-or-later

import { render, screen } from "@testing-library/react";
import { App } from "./App";

test("the shell links to its source, as the licence needs (§7 of docs/11-surface-web.md)", () => {
  render(<App />);
  expect(screen.getByRole("link", { name: "Source" }).getAttribute("href")).toBe("/about/source");
});
