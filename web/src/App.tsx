// SPDX-License-Identifier: AGPL-3.0-or-later

// The panel's shell (§1 of docs/11-surface-web.md): a client of the published API and nothing
// more. The views arrive with P1.12e; until then it says what it is and where its source is.

import { Link } from "react-aria-components";

/** The shell. */
export function App() {
  return (
    <main>
      <h1>Korpis</h1>
      <p>The panel is a client of the API and holds nothing of its own.</p>
      <Link href="/about/source">Source</Link>
    </main>
  );
}
