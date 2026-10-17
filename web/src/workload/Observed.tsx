// SPDX-License-Identifier: AGPL-3.0-or-later

// What the node observed, each value in its state (§2 of docs/11-surface-web.md): the value when
// known; the value dimmed, with how long ago it was heard, when stale; and "unknown" when
// unknown, with the last observation beside it and its age, never in its place. A resource the
// node did not report is unknown too, never 0.

import type { WorkloadStatus } from "../gen/korpis/v1/api_pb";
import { bytes } from "../i18n/format";
import { intl, t } from "../i18n/intl";
import { type Freshness, age } from "./freshness";
import { HEALTH, OBSERVED } from "./words";

/** One value, as its freshness lets it be shown. */
export function Value(props: {
  readonly value: string | undefined;
  readonly freshness: Freshness;
  readonly now: number;
}) {
  const { value, freshness: f, now } = props;
  const unknown = t({
    defaultMessage: "unknown",
    id: "OWlpMeOP",
    description: "an observed value not known",
  });
  if (value === undefined) return <span>{unknown}</span>;
  switch (f.kind) {
    case "known":
      return <span>{value}</span>;
    case "stale":
      return (
        <span className="stale">
          {value}
          <small className="aside">
            {t(
              { defaultMessage: "(heard {ago} ago)", id: "Ekt8OuWU" },
              { ago: age(now - f.heardAt) },
            )}
          </small>
        </span>
      );
    case "unknown":
      return (
        <span>
          {unknown}
          {f.observedAt !== undefined && (
            <small className="stale aside">
              {t(
                { defaultMessage: "(last observed {ago} ago: {value})", id: "YvHdWl5o" },
                { ago: age(now - f.observedAt), value },
              )}
            </small>
          )}
        </span>
      );
  }
}

/** The observed values of `status`. */
export function Observed(props: {
  readonly status: WorkloadStatus;
  readonly freshness: Freshness;
  readonly now: number;
}) {
  const o = props.status.observation;
  const r = o?.resources;
  const rows: [string, string | undefined][] = [
    [
      t({ defaultMessage: "State", id: "MddyXGDM", description: "a workload's observed state" }),
      o === undefined ? undefined : OBSERVED[o.state],
    ],
    [
      t({ defaultMessage: "Health", id: "Iqf4wUcI", description: "a workload's health" }),
      o === undefined ? undefined : HEALTH[o.health],
    ],
    [
      t({ defaultMessage: "CPU", id: "QgBuiVs5" }),
      r === undefined
        ? undefined
        : t(
            { defaultMessage: "{cores} cores", id: "99Xie+BN" },
            {
              cores: intl.formatNumber(Number(r.cpuMillis) / 1000, {
                minimumFractionDigits: 2,
                maximumFractionDigits: 2,
              }),
            },
          ),
    ],
    [
      t({ defaultMessage: "Memory", id: "dVx3yznM" }),
      r === undefined ? undefined : bytes(r.memoryBytes),
    ],
    [
      t({ defaultMessage: "Disk", id: "OuCSDnvE" }),
      r === undefined ? undefined : bytes(r.diskBytes),
    ],
  ];
  const heading = t({
    defaultMessage: "Observed",
    id: "i+q7WayW",
    description: "what the node observed",
  });
  return (
    <section aria-label={heading}>
      <h2>{heading}</h2>
      <dl>
        {rows.map(([name, value]) => (
          <div key={name}>
            <dt>{name}</dt>
            <dd>
              <Value value={value} freshness={props.freshness} now={props.now} />
            </dd>
          </div>
        ))}
      </dl>
    </section>
  );
}
