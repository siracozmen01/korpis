// SPDX-License-Identifier: AGPL-3.0-or-later

import { createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { clientsOver, ReachProvider } from "../clients";
import { RunService } from "../gen/korpis/v1/run_pb";
import { RunCause, RunStatus, RunTrigger } from "../gen/korpis/v1/workload_pb";
import { Runs } from "./Runs";

const noRelay = () => Promise.reject(new Error("no relay in this test"));
const at = (seconds: number) => ({ seconds: BigInt(seconds), nanos: 0 });

function setup(allowed?: ReadonlySet<string>) {
  const seen: string[] = [];
  let cancelled = false;
  const transport = createRouterTransport(({ service }) => {
    service(RunService, {
      listRuns: (r) => {
        seen.push(`list ${r.workload}`);
        return {
          runs: [
            {
              id: "run_2",
              workload: r.workload,
              trigger: RunTrigger.SCHEDULE,
              status: cancelled ? RunStatus.CANCELLED : RunStatus.RUNNING,
              scheduledFor: at(1_800_000_000),
              startedAt: at(1_800_000_001),
              ...(cancelled && { cause: RunCause.CANCELLED }),
            },
            {
              id: "run_1",
              workload: r.workload,
              trigger: RunTrigger.RETRY,
              attempt: 2,
              status: RunStatus.FAILED,
              cause: RunCause.EXIT,
              exitStatus: 3,
              scheduledFor: at(1_799_990_000),
              startedAt: at(1_799_990_001),
            },
            {
              id: "run_0",
              workload: r.workload,
              trigger: RunTrigger.SCHEDULE,
              status: RunStatus.MISSED,
              cause: RunCause.SKIPPED,
              occurrences: 4,
              scheduledFor: at(1_799_900_000),
            },
          ],
        };
      },
      cancelRun: (r) => {
        seen.push(`cancel ${r.run}`);
        cancelled = true;
        return {};
      },
    });
  });
  render(
    <ReachProvider
      value={{
        clients: clientsOver(transport),
        base: "",
        metered: false,
        relay: noRelay,
        ...(allowed && { allowed }),
      }}
    >
      <Runs id="wl_1" newest="run_2 2" />
    </ReachProvider>,
  );
  return seen;
}

test("a workload's runs are listed with how each ended, and an active one is cancelled", async () => {
  const seen = setup();
  expect(await screen.findByText("failed")).toBeTruthy();
  expect(screen.getByText("it exited, status 3")).toBeTruthy();
  expect(screen.getByText("retry, attempt 2")).toBeTruthy();
  // A missed run standing for several occurrences says how many, and was never started.
  expect(screen.getByText("missed while the control plane was down, 4 occurrences")).toBeTruthy();
  expect(screen.getByText("running")).toBeTruthy();
  // Only the running one is offered, and the button is its.
  fireEvent.click(screen.getByRole("button", { name: "Cancel the run" }));
  // Cancelled, as its status and as why it ended.
  expect(await screen.findAllByText("cancelled")).toHaveLength(2);
  await waitFor(() => {
    expect(screen.queryByRole("button", { name: "Cancel the run" })).toBeNull();
  });
  expect(seen).toEqual(["list wl_1", "cancel run_2", "list wl_1"]);
});

test("a run's console is opened from the list, and only its readers are offered it", async () => {
  setup();
  const consoles = await screen.findAllByRole("button", { name: "Console" });
  expect(consoles).toHaveLength(2);
  const [, failed] = consoles;
  if (failed === undefined) throw new Error("the failed run offers its console");
  fireEvent.click(failed);
  expect(await screen.findByRole("heading", { name: "Console" })).toBeTruthy();
  expect(failed.getAttribute("aria-pressed")).toBe("true");
});

test("a reader who may not stop the workload is not offered to cancel", async () => {
  setup(new Set(["workload.read", "workload.console.read"]));
  expect(await screen.findByText("running")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Cancel the run" })).toBeNull();
});
