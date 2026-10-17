// SPDX-License-Identifier: AGPL-3.0-or-later

// The convergence timeline (§3 of docs/11-surface-web.md): what was declared and when, when its
// node acknowledged it, and when it converged, each with its lag from the declaration. The
// control plane keeps no history of these moments beyond the effect log, so the panel records
// each as it hears it happen, at the time of the observation that first showed it; a moment
// that passed before the screen opened is shown as done, without a time it does not know.

import { timestampMs } from "@bufbuild/protobuf/wkt";
import { Convergence, type WorkloadStatus } from "../gen/korpis/v1/api_pb";
import { t } from "../i18n/intl";
import { declaredAt } from "./diagnosis";

/** A moment of the timeline: not yet, done at a known time, or done before it was heard. */
export type Moment =
  | { readonly kind: "pending" }
  | { readonly kind: "at"; readonly at: number }
  | { readonly kind: "before" };

/** The timeline of the newest declaration. */
export interface Timeline {
  /** What identifies the declaration: the target intent and the power version and cycle. */
  readonly key: string;
  readonly label: string;
  readonly version: string;
  readonly declared: number | undefined;
  readonly acknowledged: Moment;
  readonly converged: Moment;
}

function keyOf(s: WorkloadStatus): string {
  const p = s.workload?.power;
  return `${s.target?.id ?? ""}:${(p?.version ?? 0n).toString()}:${(p?.cycle ?? 0n).toString()}`;
}

/** What the declaration asked for, in words, and the version it is known by. */
function described(s: WorkloadStatus, prev: Timeline | undefined): [string, string] {
  const power = s.workload?.power;
  const intent = t(
    {
      defaultMessage: "intent v{version}",
      id: "wHbkqfPX",
      description: "the version of a workload's intent",
    },
    { version: (s.target?.version ?? 0n).toString() },
  );
  if (prev !== undefined && power !== undefined) {
    const [, version, cycle] = prev.key.split(":");
    if (cycle !== power.cycle.toString()) {
      return [t({ defaultMessage: "Restart requested", id: "6f0LBR2I" }), intent];
    }
    if (version !== power.version.toString())
      return [t({ defaultMessage: "Power changed", id: "kHzm3iTk" }), intent];
  }
  return [
    t({
      defaultMessage: "Declared",
      id: "YOjHY4hC",
      description: "the moment a declaration was made",
    }),
    intent,
  ];
}

/** Whether the node has taken the declaration up: converged, or a step of its plan reported. */
function acknowledged(s: WorkloadStatus): boolean {
  if (s.convergence === Convergence.CONVERGED) return true;
  const plan = s.activePlan;
  return plan !== undefined && s.observation?.planProgress?.plan === plan.id;
}

/** The moment a condition holds: kept if already recorded, else now if it newly holds. */
function moment(was: Moment | undefined, holds: boolean, at: number | undefined): Moment {
  if (was !== undefined && was.kind !== "pending") return was;
  if (!holds) return { kind: "pending" };
  return was === undefined || at === undefined ? { kind: "before" } : { kind: "at", at };
}

/** The timeline after hearing `s`, given the one before it (none on the first status heard). */
export function advance(prev: Timeline | undefined, s: WorkloadStatus): Timeline {
  const key = keyOf(s);
  const same = prev?.key === key ? prev : undefined;
  const fresh = same === undefined && prev !== undefined;
  const observed = s.observation?.observedAt;
  const at = observed === undefined ? undefined : timestampMs(observed);
  // A declaration heard being made starts from pending; one already made when the screen
  // opened has passed whatever moments it has passed, at times unknown.
  const start: Moment | undefined = fresh ? { kind: "pending" } : undefined;
  const [label, version] = same === undefined ? described(s, prev) : [same.label, same.version];
  return {
    key,
    label,
    version,
    declared: declaredAt(s),
    acknowledged: moment(same?.acknowledged ?? start, acknowledged(s), at),
    converged: moment(same?.converged ?? start, s.convergence === Convergence.CONVERGED, at),
  };
}
