// SPDX-License-Identifier: AGPL-3.0-or-later

// The convergence timeline as §3 of docs/11-surface-web.md draws it: the declaration with its time
// and version, then each moment with its lag from the declaration.

import { intl, t } from "../i18n/intl";
import { type Moment, type Timeline } from "./timeline";

function lag(m: Moment, declared: number | undefined): string {
  switch (m.kind) {
    case "pending":
      return t({
        defaultMessage: "not yet",
        id: "fKWNsalK",
        description: "a convergence moment still to come",
      });
    case "before":
      return t({
        defaultMessage: "done",
        id: "j1jIuAyi",
        description: "a convergence moment passed at a time unknown",
      });
    case "at": {
      if (declared === undefined) return intl.formatTime(m.at, { timeStyle: "medium" });
      const s = Math.max(0, m.at - declared) / 1000;
      const lag = intl.formatNumber(s, {
        style: "unit",
        unit: "second",
        unitDisplay: "narrow",
        maximumFractionDigits: s < 10 ? 1 : 0,
        minimumFractionDigits: s < 10 ? 1 : 0,
      });
      return t(
        { defaultMessage: "+{lag}", id: "2BoX8B7b", description: "time after the declaration" },
        { lag },
      );
    }
  }
}

/** The timeline. */
export function TimelineView({ timeline }: { readonly timeline: Timeline }) {
  const heading = t({
    defaultMessage: "Convergence",
    id: "CM8HNi8Q",
    description: "the convergence timeline",
  });
  return (
    <section aria-label={heading}>
      <h2>{heading}</h2>
      <table>
        <tbody>
          <tr>
            <th scope="row">{timeline.label}</th>
            <td>
              {timeline.declared === undefined
                ? ""
                : intl.formatTime(timeline.declared, { timeStyle: "medium" })}
            </td>
            <td>{timeline.version}</td>
          </tr>
          <tr>
            <th scope="row">{t({ defaultMessage: "Node acknowledged", id: "FaYLfRYl" })}</th>
            <td>{lag(timeline.acknowledged, timeline.declared)}</td>
            <td />
          </tr>
          <tr>
            <th scope="row">
              {t({
                defaultMessage: "Converged",
                id: "4H4xOTsE",
                description: "the moment a workload converged",
              })}
            </th>
            <td>{lag(timeline.converged, timeline.declared)}</td>
            <td />
          </tr>
        </tbody>
      </table>
    </section>
  );
}
