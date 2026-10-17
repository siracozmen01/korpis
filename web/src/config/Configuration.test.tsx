// SPDX-License-Identifier: AGPL-3.0-or-later

import { create } from "@bufbuild/protobuf";
import { createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { expect, test } from "vitest";
import { clientsOver, ReachProvider } from "../clients";
import { type DeclareIntentRequest, PlanService, WorkloadService } from "../gen/korpis/v1/api_pb";
import { ConfigType, FieldPermission } from "../gen/korpis/v1/recipe_pb";
import { IntentSchema, PlanApproval, PlanStatus } from "../gen/korpis/v1/workload_pb";
import { Configuration } from "./Configuration";

const noRelay = () => Promise.reject(new Error("no relay in this test"));

function setup() {
  const declared: DeclareIntentRequest[] = [];
  const asked: string[][] = [];
  const transport = createRouterTransport(({ service }) => {
    service(WorkloadService, {
      getWorkloadRecipe: (r) => {
        asked.push(r.locales);
        return {
          locale: "tr",
          metadata: { name: "paper", locale: "en" },
          config: {
            fields: [
              { key: "max_players", type: ConfigType.INTEGER, label: "Maximum players" },
              {
                key: "rcon_password",
                type: ConfigType.STRING,
                secret: true,
                permission: FieldPermission.OPERATOR,
              },
              { key: "java_heap", type: ConfigType.QUANTITY, derived: "(resources.memory * 0.8)" },
              { key: "eula", type: ConfigType.BOOLEAN, hidden: true },
            ],
          },
          messages: {
            "config.fields.max_players.label": "En fazla oyuncu",
            "metadata.description": "PaperMC sunucusu, ''Java''",
          },
        };
      },
      declareIntent: (r) => {
        declared.push(r);
        return {
          plan: {
            id: "pln_1",
            status: PlanStatus.PROPOSED,
            approval: PlanApproval.CONFIRMATION,
            steps: [{ index: 0, action: "reconfigure", reversible: true }],
            impact: { disruptive: false, dataAtRisk: [], dataAtRiskUnknown: false },
          },
        };
      },
    });
    service(PlanService, {
      approvePlan: () => ({ plan: { id: "pln_1", status: PlanStatus.APPROVED } }),
    });
  });
  const head = create(IntentSchema, {
    id: "int_2",
    version: 2n,
    body: {
      recipe: { name: "reg/paper", digest: `sha256:${"a".repeat(64)}` },
      config: {
        max_players: { kind: { case: "intValue", value: 20n } },
        java_heap: { kind: { case: "stringValue", value: "3Gi" } },
      },
    },
    withheld: ["rcon_password"],
  });
  render(
    <ReachProvider
      value={{ clients: clientsOver(transport), base: "", metered: false, relay: noRelay }}
    >
      <Configuration id="wl_1" head={head} />
    </ReachProvider>,
  );
  return { declared, asked };
}

test("the form speaks the recipe's words in the viewer's language and keeps a hidden secret", async () => {
  const { declared, asked } = setup();
  const players = await screen.findByLabelText("En fazla oyuncu");
  expect(asked[0]).toEqual([...navigator.languages]);
  expect(screen.getByText("PaperMC sunucusu, 'Java'")).toBeTruthy();
  expect(screen.getByText(/Set, and not shown to you/)).toBeTruthy();
  expect(screen.queryByText(/eula/)).toBeNull();
  expect(screen.getByText(/java_heap: 3Gi/)).toBeTruthy();
  fireEvent.change(players, { target: { value: "40" } });
  fireEvent.click(screen.getByRole("button", { name: "Save the configuration" }));
  await waitFor(() => {
    expect(declared).toHaveLength(1);
  });
  const sent = declared[0];
  if (sent === undefined) throw new Error("nothing declared");
  expect(sent.keep).toEqual(["rcon_password"]);
  expect(sent.propose).toBe(true);
  expect(sent.mutation?.expectedVersion).toBe(2n);
  const config = sent.body?.config ?? {};
  expect(config.max_players?.kind).toEqual({ case: "intValue", value: 40n });
  expect(config.java_heap, "what the control plane derives is not declared").toBeUndefined();
  expect(config.rcon_password).toBeUndefined();
});

test("a value that is not of its field's type is refused before anything is sent", async () => {
  const { declared } = setup();
  const players = await screen.findByLabelText("En fazla oyuncu");
  fireEvent.change(players, { target: { value: "forty" } });
  expect(await screen.findByText("A whole number")).toBeTruthy();
  const save = screen.getByRole("button", { name: "Save the configuration" });
  expect(save.hasAttribute("disabled") || save.getAttribute("data-disabled") === "true").toBe(true);
  expect(declared).toHaveLength(0);
});
