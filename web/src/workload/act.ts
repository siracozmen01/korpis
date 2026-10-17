// SPDX-License-Identifier: AGPL-3.0-or-later

// The calls behind a workload's actions. Each is proposed (a power change with `propose`, a
// deletion always), so the control plane computes and keeps its plan and applies nothing yet;
// the panel then confirms that very plan, by its identifier, or withdraws it. A plan previewed is
// therefore the plan that runs, and a world that moved in between is a refusal rather than a
// surprise (§4 of docs/13-surface-cli.md).

import type { Clients } from "../clients";
import type { WorkloadStatus } from "../gen/korpis/v1/api_pb";
import { PowerState, type Plan } from "../gen/korpis/v1/workload_pb";
import { t } from "../i18n/intl";

/** An action on a workload, by the action name a grant holds for it (§5 of docs/10-api.md). */
export type Action = "workload.start" | "workload.stop" | "workload.restart" | "workload.delete";

/** The words of each action's button. */
export const LABEL: Record<Action, string> = {
  "workload.start": t({
    defaultMessage: "Start",
    id: "ZE5n9bcN",
    description: "button: start a workload",
  }),
  "workload.stop": t({
    defaultMessage: "Stop",
    id: "hfrR434C",
    description: "button: stop a workload",
  }),
  "workload.restart": t({
    defaultMessage: "Restart",
    id: "/6RJEreY",
    description: "button: restart a workload",
  }),
  "workload.delete": t({
    defaultMessage: "Delete",
    id: "F1KlqJdE",
    description: "button: delete a workload",
  }),
};

/** The actions that make sense for what is declared now, among those `allowed`, if known. */
export function offered(
  status: WorkloadStatus,
  allowed: ReadonlySet<string> | undefined,
): Action[] {
  const running = status.workload?.power?.desired === PowerState.RUNNING;
  const all: Action[] = running
    ? ["workload.stop", "workload.restart", "workload.delete"]
    : ["workload.start", "workload.delete"];
  return allowed === undefined ? all : all.filter((a) => allowed.has(a));
}

const key = () => crypto.randomUUID();

/** Proposes `action` on the workload of `status`; the plan it gets waits to be confirmed. */
export async function propose(
  clients: Clients,
  status: WorkloadStatus,
  action: Action,
): Promise<Plan | undefined> {
  const workload = status.workload?.id ?? "";
  const power = { idempotencyKey: key(), expectedVersion: status.workload?.power?.version ?? 0n };
  const w = clients.workloads;
  switch (action) {
    case "workload.start":
      return (await w.start({ mutation: power, workload, propose: true })).plan;
    case "workload.stop":
      return (await w.stop({ mutation: power, workload, propose: true })).plan;
    case "workload.restart":
      return (await w.restart({ mutation: power, workload, propose: true })).plan;
    case "workload.delete":
      // A deletion is always proposed: it is irreversible (§5 of docs/10-api.md).
      return (
        await w.deleteWorkload({
          mutation: { idempotencyKey: key(), expectedVersion: status.head?.version ?? 0n },
          workload,
        })
      ).plan;
  }
}

/** Confirms the plan `plan`, which then applies. */
export async function confirm(clients: Clients, plan: Plan): Promise<Plan | undefined> {
  return (await clients.plans.approvePlan({ mutation: { idempotencyKey: key() }, plan: plan.id }))
    .plan;
}

/** Withdraws the plan `plan`, which then never applies. */
export async function withdraw(clients: Clients, plan: Plan): Promise<void> {
  await clients.plans.rejectPlan({
    mutation: { idempotencyKey: key() },
    plan: plan.id,
    reason: "withdrawn by its proposer before confirming it",
  });
}
