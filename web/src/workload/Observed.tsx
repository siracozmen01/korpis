// SPDX-License-Identifier: AGPL-3.0-or-later

// What the node observed, each value in its state (§2 of docs/11-surface-web.md): the value when
// known; the value dimmed, with how long ago it was heard, when stale; and "unknown" when
// unknown, with the last observation beside it and its age, never in its place. A resource the
// node did not report is unknown too, never 0.

import type { WorkloadStatus } from "../gen/korpis/v1/api_pb";
import { type Freshness, age } from "./freshness";
import { HEALTH, OBSERVED } from "./words";

/** One value, as its freshness lets it be shown. */
export function Value(props: {
  readonly value: string | undefined;
  readonly freshness: Freshness;
  readonly now: number;
}) {
  const { value, freshness: f, now } = props;
  if (value === undefined) return <span>unknown</span>;
  switch (f.kind) {
    case "known":
      return <span>{value}</span>;
    case "stale":
      return (
        <span className="stale">
          {value} <small>(heard {age(now - f.heardAt)} ago)</small>
        </span>
      );
    case "unknown":
      return (
        <span>
          unknown
          {f.observedAt !== undefined && (
            <small className="stale">
              {" "}
              (last observed {age(now - f.observedAt)} ago: {value})
            </small>
          )}
        </span>
      );
  }
}

const MIB = 1024 * 1024;

/** Bytes in words. */
function bytes(n: bigint): string {
  const mib = Number(n) / MIB;
  return mib >= 1024 ? `${(mib / 1024).toFixed(1)} GiB` : `${mib.toFixed(0)} MiB`;
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
    ["State", o === undefined ? undefined : OBSERVED[o.state]],
    ["Health", o === undefined ? undefined : HEALTH[o.health]],
    ["CPU", r === undefined ? undefined : `${(Number(r.cpuMillis) / 1000).toFixed(2)} cores`],
    ["Memory", r === undefined ? undefined : bytes(r.memoryBytes)],
    ["Disk", r === undefined ? undefined : bytes(r.diskBytes)],
  ];
  return (
    <section aria-label="Observed">
      <h2>Observed</h2>
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
