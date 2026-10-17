// SPDX-License-Identifier: AGPL-3.0-or-later

// The convergence timeline as §3 of docs/11-surface-web.md draws it: the declaration with its time
// and version, then each moment with its lag from the declaration.

import { type Moment, type Timeline } from "./timeline";

function lag(m: Moment, declared: number | undefined): string {
  switch (m.kind) {
    case "pending":
      return "not yet";
    case "before":
      return "done";
    case "at": {
      if (declared === undefined) return new Date(m.at).toLocaleTimeString();
      const s = Math.max(0, m.at - declared) / 1000;
      return `+${s < 10 ? s.toFixed(1) : Math.round(s).toString()}s`;
    }
  }
}

/** The timeline. */
export function TimelineView({ timeline: t }: { readonly timeline: Timeline }) {
  return (
    <section aria-label="Convergence">
      <h2>Convergence</h2>
      <table>
        <tbody>
          <tr>
            <th scope="row">{t.label}</th>
            <td>{t.declared === undefined ? "" : new Date(t.declared).toLocaleTimeString()}</td>
            <td>{t.version}</td>
          </tr>
          <tr>
            <th scope="row">Node acknowledged</th>
            <td>{lag(t.acknowledged, t.declared)}</td>
            <td />
          </tr>
          <tr>
            <th scope="row">Converged</th>
            <td>{lag(t.converged, t.declared)}</td>
            <td />
          </tr>
        </tbody>
      </table>
    </section>
  );
}
