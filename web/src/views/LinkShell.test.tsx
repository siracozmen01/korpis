// SPDX-License-Identifier: AGPL-3.0-or-later

import { createRouterTransport } from "@connectrpc/connect";
import { render, screen } from "@testing-library/react";
import { clientsOver } from "../clients";
import { GrantService, WorkloadService } from "../gen/korpis/v1/api_pb";
import { GrantState } from "../gen/korpis/v1/authority_pb";
import { PowerState } from "../gen/korpis/v1/workload_pb";
import { LinkShell } from "./LinkShell";

test("a share link's screen holds exactly its workload and says what the link allows", async () => {
  const watched: string[] = [];
  const transport = createRouterTransport(({ service }) => {
    service(GrantService, {
      listGrants: () => ({
        grants: [
          {
            scope: { kind: { case: "workload", value: "wl_b" } },
            actions: ["workload.read", "workload.restart"],
            conditions: { maxUses: 3n },
            linkForm: true,
            state: GrantState.ISSUED,
          },
        ],
      }),
    });
    service(WorkloadService, {
      async *watchWorkload(r) {
        watched.push(r.workload);
        yield {
          status: {
            workload: {
              id: r.workload,
              name: "mc-survival",
              power: { desired: PowerState.RUNNING },
            },
          },
        };
        await new Promise(() => undefined);
      },
    });
  });
  render(<LinkShell clients={clientsOver(transport)} path="/g/" />);
  expect(await screen.findByRole("heading", { name: "mc-survival" })).toBeTruthy();
  expect(
    screen.getByText(/lets you see its status, restart it\. It can be used 3 times/),
  ).toBeTruthy();
  expect(screen.getByText("Declared: running")).toBeTruthy();
  expect(watched).toEqual(["wl_b"]);
});

test("without the link in memory the screen asks for it again rather than failing", () => {
  render(<LinkShell clients={undefined} path="/g/" />);
  expect(screen.getByRole("heading", { name: "Open the link again" })).toBeTruthy();
});
