// SPDX-License-Identifier: AGPL-3.0-or-later

// Which stage a workload that has not converged is at (§3 of docs/11-surface-web.md): "declared
// 40 seconds ago, node fra-3 has not acknowledged" is a diagnosis, a greyed-out button is not. Each
// sentence is read from the status the control plane computed (§4.3 of docs/02-architecture.md),
// never guessed: the stage is the convergence it reports, and the detail what the status holds.

import { timestampMs } from "@bufbuild/protobuf/wkt";
import { Convergence, type WorkloadStatus } from "../gen/korpis/v1/api_pb";
import { ObservedState } from "../gen/korpis/v1/workload_pb";
import { age } from "./freshness";
import { UNSATISFIABLE } from "./words";

/** The node a workload is placed on, as far as the caller may read it. */
export function nodeOf(status: WorkloadStatus): string {
  const p = status.placement;
  if (p === undefined) return "its node";
  if (p.node !== "") return `node ${p.node}`;
  return p.region === "" ? "its node" : `its node in ${p.region}`;
}

/** When what it converges to was declared: the approved intent's or the power's, the later. */
export function declaredAt(status: WorkloadStatus): number | undefined {
  const times = [status.target?.declaredAt, status.workload?.power?.setAt].flatMap((t) =>
    t === undefined ? [] : [timestampMs(t)],
  );
  return times.length === 0 ? undefined : Math.max(...times);
}

/** Why no node took it, from the newest attempt's explanation. */
function unplaced(status: WorkloadStatus): string {
  const rejected = status.unplaced?.explanation?.rejected ?? [];
  if (rejected.length === 0) return "No node can take it.";
  const checks = rejected.map((c) =>
    c.nodes === undefined ? c.check : `${c.check} (${c.nodes.toString()} nodes)`,
  );
  return `No node can take it: ${checks.join(", ")}.`;
}

/** Why the node cannot satisfy what it holds. */
function failing(status: WorkloadStatus): string {
  const o = status.observation;
  const node = nodeOf(status);
  if (o === undefined) return `${node} cannot run it.`;
  if (o.state === ObservedState.CRASHED) {
    const code = o.exitCode === undefined ? "" : `, exit code ${o.exitCode.toString()}`;
    return `It crashed on ${node}${code}.`;
  }
  const why = o.reasonDetail !== "" ? o.reasonDetail : UNSATISFIABLE[o.reason];
  return `${node} holds it and cannot run it: ${why}.`;
}

/** What it is waiting on while it applies. */
function applying(status: WorkloadStatus, now: number): string {
  const plan = status.activePlan;
  const progress = status.observation?.planProgress;
  const node = nodeOf(status);
  if (plan !== undefined && progress?.plan === plan.id) {
    return `${node} is applying step ${(progress.step + 1).toString()} of ${plan.steps.length.toString()}.`;
  }
  const at = declaredAt(status);
  const since = at === undefined ? "Declared" : `Declared ${age(now - at)} ago`;
  return `${since}, ${node} has not acknowledged.`;
}

/** The stage a workload is at, in a sentence; nothing once it has converged. */
export function diagnosis(status: WorkloadStatus, now: number): string | undefined {
  const said = stage(status, now);
  return said === undefined ? undefined : said.charAt(0).toUpperCase() + said.slice(1);
}

function stage(status: WorkloadStatus, now: number): string | undefined {
  switch (status.convergence) {
    case Convergence.CONVERGED:
      return undefined;
    case Convergence.APPLYING:
      return applying(status, now);
    case Convergence.FAILING:
      return failing(status);
    case Convergence.UNPLACED:
      return unplaced(status);
    case Convergence.AWAITING_APPROVAL:
      return "Its plan waits for approval.";
    case Convergence.SCHEDULED: {
      const after = status.activePlan?.applyAfter;
      return after === undefined
        ? "Its plan is scheduled."
        : `Its plan applies at ${new Date(timestampMs(after)).toLocaleString()}.`;
    }
    case Convergence.UNKNOWN:
    case Convergence.UNSPECIFIED: {
      const o = status.observation;
      if (o?.observedAt === undefined) return "Nothing has been observed of it yet.";
      return `${nodeOf(status)} has not been heard for ${age(now - timestampMs(o.observedAt))}.`;
    }
  }
}
