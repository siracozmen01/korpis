// SPDX-License-Identifier: AGPL-3.0-or-later

import { createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { clientsOver, ReachProvider } from "../clients";
import { EffectService } from "../gen/korpis/v1/api_pb";
import { EffectOutcome } from "../gen/korpis/v1/workload_pb";
import { Audit } from "./Audit";

const noRelay = () => Promise.reject(new Error("no relay in this test"));

/** An organization's log of two pages: a restart a person relayed through a bot, a refusal
 * repeated, and then the creation of a project. */
function reach(asked: string[]) {
  const transport = createRouterTransport(({ service }) => {
    service(EffectService, {
      listEffects: (r) => {
        asked.push(r.organization ?? "");
        if (r.page?.pageToken !== "") {
          return {
            effects: [
              {
                id: "eff_3",
                action: "project.create",
                outcome: EffectOutcome.SUCCEEDED,
                actor: { kind: { case: "user", value: "usr_ada" } },
              },
            ],
          };
        }
        return {
          effects: [
            {
              id: "eff_1",
              action: "workload.restart",
              outcome: EffectOutcome.SUCCEEDED,
              actor: {
                kind: {
                  case: "external",
                  value: {
                    provider: "org_a/discord",
                    selector: { case: "user", value: "123" },
                  },
                },
              },
              relay: { kind: { case: "extension", value: "ext_bot" } },
            },
            {
              id: "eff_2",
              action: "org.write",
              outcome: EffectOutcome.DENIED,
              actor: { kind: { case: "token", value: "tok_ci" } },
              count: 3n,
            },
          ],
          page: { nextPageToken: "sealed" },
        };
      },
    });
  });
  return { clients: clientsOver(transport), base: "", metered: false, relay: noRelay };
}

test("an organization's trail names who did what, through whom, and marks a refusal", async () => {
  const asked: string[] = [];
  render(
    <ReachProvider value={reach(asked)}>
      <Audit id="org_a" />
    </ReachProvider>,
  );
  const trail = await screen.findByRole("region", { name: "Audit" });
  const restart = within(trail).getByText("workload.restart").closest("li");
  expect(restart?.textContent).toContain("by user:123 at org_a/discord, through extension ext_bot");
  const refused = within(trail).getByText("org.write").closest("li");
  expect(refused?.className).toContain("refused");
  expect(refused?.textContent).toContain("refused");
  expect(refused?.textContent).toContain("3 times");

  fireEvent.click(within(trail).getByRole("button", { name: "Show more" }));
  expect(await within(trail).findByText("project.create")).toBeTruthy();
  expect(asked).toEqual(["org_a", "org_a"]);
});
