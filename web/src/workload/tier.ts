// SPDX-License-Identifier: AGPL-3.0-or-later

// How much a plan interrupts (§4 of docs/11-surface-web.md). The plan is always computed and
// kept; what scales with its risk is the interruption. A plan that is reversible, touches one
// workload, and loses nothing is applied at once and shown after; one that interrupts service or
// touches several workloads is previewed in place with one confirmation; one that destroys data,
// or cannot be undone, or whose risk to data is unknown, gets the whole plan on its own screen,
// every affected object listed, and an explicit confirmation. Each test reads what the control
// plane classified (the steps' `reversible` and `disruptive`, the impact), never the action's
// name: a recipe may reclassify a step (§3.3 of docs/01-model.md).

import { ObjectKind } from "../gen/korpis/v1/common_pb";
import type { Plan } from "../gen/korpis/v1/workload_pb";

/** The interruption a plan calls for. */
export type Tier = "direct" | "confirm" | "full";

/** The workloads a plan touches: its own, and any other its steps target. */
function workloads(plan: Plan): number {
  const ids = new Set([plan.workload]);
  for (const s of plan.steps) {
    if (s.target?.kind === ObjectKind.WORKLOAD) ids.add(s.target.id);
  }
  return ids.size;
}

/** The interruption `plan` calls for. */
export function tierOf(plan: Plan): Tier {
  const impact = plan.impact;
  const atRisk = impact === undefined || impact.dataAtRisk.length > 0 || impact.dataAtRiskUnknown;
  if (atRisk || plan.steps.some((s) => !s.reversible)) return "full";
  const disruptive = impact.disruptive || plan.steps.some((s) => s.disruptive);
  if (disruptive || workloads(plan) > 1) return "confirm";
  return "direct";
}
