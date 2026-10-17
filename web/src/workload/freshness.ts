// SPDX-License-Identifier: AGPL-3.0-or-later

// The three states of an observed value (§2 of docs/11-surface-web.md). Known: the control plane
// holds a current observation and the panel is hearing it live. Stale: the panel has stopped
// hearing, so what it shows is as old as the last thing it heard, and it says how old. Unknown:
// nothing was ever observed, or the control plane says the observation is not current (its node
// is not heard, §4.3 of docs/02-architecture.md); the last one is shown with its time, never as
// now.

import { timestampMs } from "@bufbuild/protobuf/wkt";
import { Convergence, type WorkloadStatus } from "../gen/korpis/v1/api_pb";
import { ObservedState } from "../gen/korpis/v1/workload_pb";
import { intl } from "../i18n/intl";

/** Whether the panel is hearing the workload, and when it last heard. */
export interface Heard {
  readonly live: boolean;
  readonly at: number;
}

/** How far an observed value can be believed. */
export type Freshness =
  | { readonly kind: "known" }
  | { readonly kind: "stale"; readonly heardAt: number }
  | { readonly kind: "unknown"; readonly observedAt: number | undefined };

/** The freshness of `status`'s observation, as heard. */
export function freshness(status: WorkloadStatus, heard: Heard): Freshness {
  const o = status.observation;
  if (o === undefined) return { kind: "unknown", observedAt: undefined };
  const current = status.convergence !== Convergence.UNKNOWN && o.state !== ObservedState.UNKNOWN;
  if (!current) {
    const at = o.observedAt === undefined ? undefined : timestampMs(o.observedAt);
    return { kind: "unknown", observedAt: at };
  }
  return heard.live ? { kind: "known" } : { kind: "stale", heardAt: heard.at };
}

/** A span of milliseconds in the panel's locale's words: seconds, minutes, hours, or days. */
export function age(ms: number): string {
  const span = (value: number, unit: string) =>
    intl.formatNumber(value, { style: "unit", unit, unitDisplay: "short" });
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return span(s, "second");
  const m = Math.floor(s / 60);
  if (m < 60) return span(m, "minute");
  const h = Math.floor(m / 60);
  if (h < 48) return span(h, "hour");
  return span(Math.floor(h / 24), "day");
}
