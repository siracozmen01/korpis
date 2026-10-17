// SPDX-License-Identifier: AGPL-3.0-or-later

import { create, type MessageInitShape } from "@bufbuild/protobuf";
import { createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen } from "@testing-library/react";
import { clientsOver, ReachProvider } from "../clients";
import { PlanService, WorkloadService, WorkloadStatusSchema } from "../gen/korpis/v1/api_pb";
import {
  PlanApproval,
  PlanStatus,
  PowerState,
  type PlanSchema,
} from "../gen/korpis/v1/workload_pb";
import { Actions } from "./Actions";

type PlanInit = MessageInitShape<typeof PlanSchema>;
const safe = { disruptive: false, dataAtRisk: [], dataAtRiskUnknown: false };
const proposed = (init: PlanInit): PlanInit => ({
  id: "pln_1",
  workload: "wl_1",
  status: PlanStatus.PROPOSED,
  approval: PlanApproval.CONFIRMATION,
  ...init,
});
const START = proposed({ steps: [{ index: 0, action: "start", reversible: true }], impact: safe });
const RESTART = proposed({
  steps: [{ index: 0, action: "restart", reversible: true, disruptive: true }],
  impact: { ...safe, disruptive: true },
});
const DELETE = proposed({
  steps: [{ index: 0, action: "retire", reversible: false, disruptive: true }],
  impact: { ...safe, disruptive: true, dataAtRisk: ["world"] },
});

function setup(desired: PowerState, allowed?: ReadonlySet<string>) {
  const seen: string[] = [];
  const transport = createRouterTransport(({ service }) => {
    service(WorkloadService, {
      start: (r) => (seen.push(`start propose=${String(r.propose)}`), { plan: START }),
      restart: (r) => (seen.push(`restart propose=${String(r.propose)}`), { plan: RESTART }),
      deleteWorkload: () => (seen.push("delete"), { plan: DELETE }),
    });
    service(PlanService, {
      approvePlan: (r) => (
        seen.push(`approve ${r.plan}`),
        { plan: { ...START, status: PlanStatus.APPROVED } }
      ),
      rejectPlan: (r) => (seen.push(`reject ${r.plan}`), {}),
    });
  });
  const status = create(WorkloadStatusSchema, {
    workload: { id: "wl_1", power: { desired, version: 2n } },
  });
  render(
    <ReachProvider
      value={{
        clients: clientsOver(transport),
        base: "",
        metered: false,
        ...(allowed && { allowed }),
      }}
    >
      <Actions status={status} />
    </ReachProvider>,
  );
  return seen;
}

test("a plan that interrupts nothing is confirmed at once and shown after", async () => {
  const seen = setup(PowerState.STOPPED);
  fireEvent.click(screen.getByRole("button", { name: "Start" }));
  expect(await screen.findByText("Start: done. What it did")).toBeTruthy();
  expect(seen).toEqual(["start propose=true", "approve pln_1"]);
});

test("a plan that interrupts service is previewed in place, and cancelling withdraws it", async () => {
  const seen = setup(PowerState.RUNNING);
  fireEvent.click(screen.getByRole("button", { name: "Restart" }));
  expect(await screen.findByText("Restart will do this:")).toBeTruthy();
  expect(screen.getByText(/interrupts service/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(await screen.findByRole("button", { name: "Restart" })).toHaveProperty("disabled", false);
  expect(seen).toEqual(["restart propose=true", "reject pln_1"]);
});

test("a plan that destroys data gets its own screen and an explicit confirmation", async () => {
  const seen = setup(PowerState.RUNNING);
  fireEvent.click(screen.getByRole("button", { name: "Delete" }));
  expect(await screen.findByRole("heading", { name: "Review: Delete" })).toBeTruthy();
  expect(screen.getByText("Data at risk: world.")).toBeTruthy();
  expect(screen.getByText(/IRREVERSIBLE/)).toBeTruthy();
  const go = screen.getByRole("button", { name: "Delete" });
  expect(go).toHaveProperty("disabled", true);
  fireEvent.click(screen.getByRole("checkbox"));
  fireEvent.click(go);
  expect(await screen.findByText("Delete: done. What it did")).toBeTruthy();
  expect(seen).toEqual(["delete", "approve pln_1"]);
});

test("a share link is offered only the actions it holds", () => {
  setup(PowerState.RUNNING, new Set(["workload.restart"]));
  expect(screen.getAllByRole("button").map((b) => b.textContent)).toEqual(["Restart"]);
});
