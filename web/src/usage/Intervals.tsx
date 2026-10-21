// SPDX-License-Identifier: AGPL-3.0-or-later

// Each record of a window, on request (§2 of docs/15-observability.md): the interval it covers,
// whether it was measured, is a gap, or arrived after its day closed, why where it says, and its
// quantities, "-" where nothing measured one, never a zero.

import type { ReactNode } from "react";
import { type UsageRecord, UsageKind } from "../gen/korpis/v1/metering_pb";
import { bytes } from "../i18n/format";
import { intl, t } from "../i18n/intl";
import { cpuSeconds, gibHours, spanMs } from "./figures";

function kindText(k: UsageKind): string {
  switch (k) {
    case UsageKind.GAP:
      return t({ defaultMessage: "unheard", id: "a2d2NBD2", description: "a metering gap" });
    case UsageKind.ADJUSTMENT:
      return t({
        defaultMessage: "late, after its day closed",
        id: "FC/2C3Mk",
        description: "a metering adjustment",
      });
    default:
      return t({ defaultMessage: "measured", id: "5TdsCzpq", description: "a metering record" });
  }
}

const shown = <T,>(v: T | undefined, f: (v: T) => string) => (v === undefined ? "-" : f(v));

function time(ms: number): string {
  return intl.formatDate(new Date(ms), { dateStyle: "short", timeStyle: "short" });
}

/** One record. */
function Row({ record: r }: { readonly record: UsageRecord }) {
  const span = spanMs(r);
  const cells: ReactNode[] = [
    t(
      { defaultMessage: "{from} to {to}", id: "DFOWqdkl", description: "an interval" },
      { from: time(span.from), to: time(span.to) },
    ),
    r.cause === "" ? kindText(r.kind) : `${kindText(r.kind)}: ${r.cause}`,
    shown(r.cpuUsec, cpuSeconds),
    shown(r.memoryKibSeconds, gibHours),
    shown(r.memoryPeakBytes, bytes),
    shown(r.ioReadBytes, bytes),
    shown(r.ioWriteBytes, bytes),
    shown(r.netRxBytes, bytes),
    shown(r.netTxBytes, bytes),
    shown(r.egressBytes, bytes),
  ];
  return (
    <tr className={r.kind === UsageKind.GAP ? "gap" : undefined}>
      {cells.map((c, i) => (
        <td key={i}>{c}</td>
      ))}
    </tr>
  );
}

/** The records, each a row. */
export function Intervals({ records }: { readonly records: readonly UsageRecord[] }) {
  if (records.length === 0) {
    return <p>{t({ defaultMessage: "Nothing was recorded over the day.", id: "EYSPop3r" })}</p>;
  }
  const header = [
    t({ defaultMessage: "Interval", id: "DTF6uJd7", description: "a usage column" }),
    t({ defaultMessage: "Recorded as", id: "dtYTi3e8", description: "a usage column" }),
    t({ defaultMessage: "CPU", id: "ipeOHj31", description: "a usage column" }),
    t({ defaultMessage: "Memory", id: "lO2mTRKT", description: "a usage column" }),
    t({ defaultMessage: "Peak", id: "rtuT1XEt", description: "a usage column" }),
    t({ defaultMessage: "Read", id: "QaevgwhF", description: "a usage column" }),
    t({ defaultMessage: "Written", id: "SqsiTxuN", description: "a usage column" }),
    t({ defaultMessage: "Received", id: "nN6WZyJ2", description: "a usage column" }),
    t({ defaultMessage: "Sent", id: "JegOZ5Sz", description: "a usage column" }),
    t({
      defaultMessage: "Egress",
      id: "GUVPdT95",
      description: "a usage column: sent beyond the node",
    }),
  ];
  return (
    <details>
      <summary>
        {t(
          {
            defaultMessage: "{n, plural, one {The # record} other {Each of the # records}}",
            id: "hlu/UVW6",
          },
          { n: records.length },
        )}
      </summary>
      <table>
        <thead>
          <tr>
            {header.map((h) => (
              <th key={h}>{h}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {records.map((r) => (
            <Row key={r.id} record={r} />
          ))}
        </tbody>
      </table>
    </details>
  );
}
