// SPDX-License-Identifier: AGPL-3.0-or-later

import { createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { clientsOver, ReachProvider } from "../clients";
import { GrantService, OrganizationService, type SetQuotaRequest } from "../gen/korpis/v1/api_pb";
import { GrantState } from "../gen/korpis/v1/authority_pb";
import type { Organization } from "../gen/korpis/v1/tenancy_pb";
import { QuotaView } from "./Quota";

const noRelay = () => Promise.reject(new Error("no relay in this test"));

/** The reseller's set: four workloads guaranteed, ten sold, held two and five. */
const quota = {
  entries: [
    {
      reservationBounds: { workloads: 4n },
      limitBounds: { workloads: 10n },
      held: { reservations: { workloads: 2n }, limits: { workloads: 5n }, unbounded: [] },
    },
  ],
  childOvercommitPercent: { workloads: 150 },
};

const reseller = { id: "org_r", name: "reseller", parent: "org_h", quota };

/** A reach whose caller holds `actions` on the host, `org_h`, above the reseller, which has
 * one customer given three workloads guaranteed and six sold. */
function reach(actions: string[], asked: SetQuotaRequest[] = []) {
  const customer = {
    id: "org_c",
    name: "customer",
    parent: "org_r",
    quota: { entries: [{ reservationBounds: { workloads: 3n }, limitBounds: { workloads: 6n } }] },
  };
  const transport = createRouterTransport(({ service }) => {
    service(GrantService, {
      listGrants: () => ({
        grants: [
          {
            actions,
            scope: { kind: { case: "organization", value: "org_h" } },
            state: GrantState.ISSUED,
          },
        ],
      }),
    });
    service(OrganizationService, {
      getOrganization: (r) => ({
        organization: r.id === "org_c" ? customer : { id: r.id, name: r.id },
      }),
      listOrganizations: () => ({ organizations: [customer] }),
      setQuota: (r) => {
        asked.push(r);
        return { organization: { ...reseller, quota: r.quota } };
      },
    });
  });
  return { clients: clientsOver(transport), base: "", metered: false, relay: noRelay };
}

function shown(organization: Partial<Organization>, value: ReturnType<typeof reach>) {
  render(
    <ReachProvider value={value}>
      <QuotaView organization={{ ...reseller, ...organization } as Organization} />
    </ReachProvider>,
  );
}

test("a quota shows its bounds, what is held of them, and what may be sold below", async () => {
  shown({}, reach([]));
  const row = screen.getByRole("row", { name: /Workloads/ });
  // Guaranteed 4, reserved now 2, sold 10, limited now 5, may sell 15 at 150 percent.
  expect(
    within(row)
      .getAllByRole("cell")
      .map((c) => c.textContent),
  ).toEqual(["every workload", "4", "2", "10", "5", "15"]);
  fireEvent.click(screen.getByRole("button", { name: "Show what it gave below" }));
  const below = await screen.findByRole("columnheader", { name: "Sold below" });
  expect(below).toBeTruthy();
  const own = screen.getByRole("table", { name: "Its quota" });
  const cells = within(within(own).getByRole("row", { name: /Workloads/ })).getAllByRole("cell");
  expect(cells.slice(-2).map((c) => c.textContent)).toEqual(["3", "6"]);
  // And what the customer was given and holds, beside its name.
  const customer = within(screen.getByRole("table", { name: "Below it" })).getByRole("row", {
    name: /customer/,
  });
  expect(
    within(customer)
      .getAllByRole("cell")
      .map((c) => c.textContent),
  ).toEqual(["every workload", "Workloads", "3", "0", "6", "0"]);
});

test("an organization without a quota says so, and a root holds none", () => {
  shown({ quota: undefined }, reach([]));
  expect(screen.getByText(/No quota is set here/)).toBeTruthy();
  shown({ parent: undefined }, reach([]));
  expect(screen.getByText(/A root organization holds no quota/)).toBeTruthy();
});

test("a caller holding the quota above sets it, and a bound that is not a number is named", async () => {
  const asked: SetQuotaRequest[] = [];
  shown({}, reach(["org.quota.write"], asked));
  fireEvent.click(await screen.findByRole("button", { name: "Set its quota" }));
  const sold = screen.getByRole("textbox", { name: "Entry 1, sold Workloads" });
  fireEvent.change(sold, { target: { value: "lots" } });
  fireEvent.click(screen.getByRole("button", { name: "Save the quota" }));
  expect((await screen.findByRole("alert")).textContent).toBe(
    "Entry 1: Workloads is not a whole number.",
  );
  fireEvent.change(sold, { target: { value: "12" } });
  fireEvent.click(screen.getByRole("button", { name: "Save the quota" }));
  expect(await screen.findByRole("button", { name: "Set its quota" })).toBeTruthy();
  expect(asked.map((r) => [r.organization, r.quota?.entries[0]?.limitBounds?.workloads])).toEqual([
    ["org_r", 12n],
  ]);
  expect(asked[0]?.quota?.childOvercommitPercent).toEqual({ workloads: 150 });
});

test("no editor is offered to a caller holding nothing above", async () => {
  shown({}, reach(["org.read"]));
  await new Promise((settle) => setTimeout(settle, 50));
  expect(screen.queryByRole("button", { name: "Set its quota" })).toBeNull();
});
