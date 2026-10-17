// SPDX-License-Identifier: AGPL-3.0-or-later

import { create, type MessageInitShape } from "@bufbuild/protobuf";
import { timestampFromMs } from "@bufbuild/protobuf/wkt";
import { Convergence, WorkloadStatusSchema } from "../gen/korpis/v1/api_pb";
import { ObservedState } from "../gen/korpis/v1/workload_pb";
import { diagnosis } from "./diagnosis";
import { age, freshness } from "./freshness";
import { advance } from "./timeline";

const T = 1_700_000_000_000;
const status = (init: MessageInitShape<typeof WorkloadStatusSchema>) =>
  create(WorkloadStatusSchema, init);
const observed = (at: number, state = ObservedState.RUNNING) => ({
  observedAt: timestampFromMs(at),
  state,
});

test("a value is known while current and heard, stale once not heard, unknown when not current", () => {
  const current = status({ convergence: Convergence.CONVERGED, observation: observed(T) });
  expect(freshness(current, { live: true, at: T })).toEqual({ kind: "known" });
  expect(freshness(current, { live: false, at: T + 5 })).toEqual({ kind: "stale", heardAt: T + 5 });
  const silent = status({ convergence: Convergence.UNKNOWN, observation: observed(T) });
  expect(freshness(silent, { live: true, at: T + 9 })).toEqual({ kind: "unknown", observedAt: T });
  expect(freshness(status({}), { live: true, at: T })).toEqual({
    kind: "unknown",
    observedAt: undefined,
  });
  expect([age(4_400), age(125_000), age(3 * 3_600_000), age(72 * 3_600_000)]).toEqual([
    "4 sec",
    "2 min",
    "3 hr",
    "3 days",
  ]);
});

test("each stage short of converged is said from what the status holds", () => {
  const placed = { node: "fra-3", region: "eu" };
  const declared = { declaredAt: timestampFromMs(T), version: 41n, id: "int_1" };
  const waiting = status({
    convergence: Convergence.APPLYING,
    placement: placed,
    target: declared,
  });
  expect(diagnosis(waiting, T + 40_000)).toBe(
    "Declared 40 sec ago, node fra-3 has not acknowledged.",
  );
  const stepping = status({
    convergence: Convergence.APPLYING,
    placement: { node: "", region: "eu" },
    activePlan: { id: "pln_1", steps: [{}, {}, {}] },
    observation: { planProgress: { plan: "pln_1", step: 1 } },
  });
  expect(diagnosis(stepping, T)).toBe("Its node in eu is applying step 2 of 3.");
  const crashed = status({
    convergence: Convergence.FAILING,
    placement: placed,
    observation: { ...observed(T, ObservedState.CRASHED), exitCode: 137 },
  });
  expect(diagnosis(crashed, T)).toBe("It crashed on node fra-3, exit code 137.");
  const nowhere = status({
    convergence: Convergence.UNPLACED,
    unplaced: { explanation: { rejected: [{ check: "memory", nodes: 2 }] } },
  });
  expect(diagnosis(nowhere, T)).toBe("No node can take it: memory (2 nodes).");
  const silent = status({
    convergence: Convergence.UNKNOWN,
    placement: placed,
    observation: observed(T),
  });
  expect(diagnosis(silent, T + 90_000)).toBe("Node fra-3 has not been heard for 1 min.");
  expect(diagnosis(status({ convergence: Convergence.CONVERGED }), T)).toBeUndefined();
});

test("the timeline times what it hears happen and marks what passed before as done", () => {
  const target = { id: "int_1", version: 41n, declaredAt: timestampFromMs(T) };
  const power = (cycle: bigint) => ({ version: 3n, cycle, setAt: timestampFromMs(T + 1_000) });
  const opened = advance(
    undefined,
    status({ convergence: Convergence.CONVERGED, target, workload: { power: power(0n) } }),
  );
  expect([opened.label, opened.acknowledged.kind, opened.converged.kind]).toEqual([
    "Declared",
    "before",
    "before",
  ]);
  const restart = status({
    convergence: Convergence.APPLYING,
    target,
    workload: { power: power(1n) },
    activePlan: { id: "pln_2" },
    observation: { ...observed(T + 1_300), planProgress: { plan: "pln_2", step: 0 } },
  });
  const acked = advance(opened, restart);
  expect([acked.label, acked.declared, acked.acknowledged, acked.converged]).toEqual([
    "Restart requested",
    T + 1_000,
    { kind: "at", at: T + 1_300 },
    { kind: "pending" },
  ]);
  const done = advance(
    acked,
    status({
      convergence: Convergence.CONVERGED,
      target,
      workload: { power: power(1n) },
      observation: observed(T + 13_000),
    }),
  );
  expect([done.acknowledged, done.converged]).toEqual([
    { kind: "at", at: T + 1_300 },
    { kind: "at", at: T + 13_000 },
  ]);
});
