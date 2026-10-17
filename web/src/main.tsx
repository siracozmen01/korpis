// SPDX-License-Identifier: AGPL-3.0-or-later

// The panel's entry: the shell, mounted on the page's one root.

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./styles.css";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("the page has no #root to mount the panel on");
}
createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
