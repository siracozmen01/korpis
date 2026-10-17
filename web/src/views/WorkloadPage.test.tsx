// SPDX-License-Identifier: AGPL-3.0-or-later

import { timestampFromMs } from "@bufbuild/protobuf/wkt";
import { createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen } from "@testing-library/react";
import { clientsOver, ReachProvider } from "../clients";
import { Convergence, WorkloadService } from "../gen/korpis/v1/api_pb";
import { ObservedState, PowerState } from "../gen/korpis/v1/workload_pb";
import { WorkloadPage } from "./WorkloadPage";

test("the screen follows the workload live, and says when it no longer is", async () => {
  const now = Date.now();
  let opened = 0;
  let converge: () => void = () => undefined;
  const transport = createRouterTransport(({ service }) => {
    service(WorkloadService, {
      async *watchWorkload() {
        opened += 1;
        const workload = { id: "wl_1", name: "mc", power: { desired: PowerState.RUNNING } };
        const target = { id: "int_1", version: 41n, declaredAt: timestampFromMs(now - 40_000) };
        const placement = { node: "fra-3" };
        yield { status: { workload, target, placement, convergence: Convergence.APPLYING } };
        await new Promise<void>((resolve) => {
          converge = resolve;
        });
        const observation = {
          observedAt: timestampFromMs(now),
          state: ObservedState.RUNNING,
          resources: { memoryBytes: 512n * 1024n * 1024n },
        };
        yield {
          status: { workload, target, placement, observation, convergence: Convergence.CONVERGED },
        };
      },
    });
  });
  render(
    <ReachProvider value={{ clients: clientsOver(transport), base: "/g", metered: true }}>
      <WorkloadPage id="wl_1" />
    </ReachProvider>,
  );
  expect(await screen.findByText(/node fra-3 has not acknowledged/)).toBeTruthy();
  expect(screen.getByText("Declared running, applying")).toBeTruthy();
  expect(screen.getAllByText("unknown").length).toBeGreaterThan(0);
  converge();
  expect(await screen.findByText("Declared running, converged")).toBeTruthy();
  expect(screen.getByText("512 MiB")).toBeTruthy();
  expect(await screen.findByText(/Not live: last heard/)).toBeTruthy();
  expect(screen.getByText(/512 MiB/).className).toBe("stale");
  fireEvent.click(screen.getByRole("button", { name: /Open it again/ }));
  expect(await screen.findByText(/node fra-3 has not acknowledged/)).toBeTruthy();
  expect(opened).toBe(2);
});
