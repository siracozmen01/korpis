// SPDX-License-Identifier: AGPL-3.0-or-later

// The panel's entry: the shell, mounted on the page's one root.

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { intl } from "./i18n/intl";
import { register } from "./sw/register";
import "./styles.css";

// The page says the language it speaks, for a screen reader's voice and the browser's hyphenation.
document.documentElement.lang = intl.locale;
const root = document.getElementById("root");
if (root === null) {
  throw new Error("the page has no #root to mount the panel on");
}
register();
createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
