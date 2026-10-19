// SPDX-License-Identifier: AGPL-3.0-or-later

import { createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen } from "@testing-library/react";
import { clientsOver, ReachProvider } from "../clients";
import {
  GrantService,
  OrganizationService,
  type SuspendOrganizationRequest,
} from "../gen/korpis/v1/api_pb";
import { GrantState } from "../gen/korpis/v1/authority_pb";
import { OnSuspend, type Organization, OrganizationStatus } from "../gen/korpis/v1/tenancy_pb";
import { Suspension } from "./Suspension";

const noRelay = () => Promise.reject(new Error("no relay in this test"));

const customer = {
  id: "org_c",
  name: "customer",
  parent: "org_r",
  status: OrganizationStatus.ACTIVE,
};

/** A reach whose caller holds `actions` on the reseller, `org_r`, above the customer. */
function reach(actions: string[], asked: SuspendOrganizationRequest[] = [], link = false) {
  const transport = createRouterTransport(({ service }) => {
    service(GrantService, {
      listGrants: () => ({
        grants: [
          {
            actions,
            scope: { kind: { case: "organization", value: "org_r" } },
            state: GrantState.ISSUED,
          },
        ],
      }),
    });
    service(OrganizationService, {
      getOrganization: (r) => ({ organization: { id: r.id, name: r.id } }),
      suspendOrganization: (r) => {
        asked.push(r);
        const suspension = { reason: r.reason, onSuspend: r.onSuspend };
        return {
          organization: { ...customer, status: OrganizationStatus.SUSPENDED, suspension },
        };
      },
    });
  });
  const allowed = link ? new Set(["workload.read"]) : undefined;
  return {
    clients: clientsOver(transport),
    base: "",
    metered: link,
    relay: noRelay,
    ...(allowed === undefined ? {} : { allowed }),
  };
}

function shown(organization: Partial<Organization>, value: ReturnType<typeof reach>) {
  render(
    <ReachProvider value={value}>
      <Suspension organization={{ ...customer, ...organization } as Organization} />
    </ReachProvider>,
  );
}

test("a suspended organization says why, what became of its workloads, and what stays readable", async () => {
  const suspension = { reason: "the invoice of October is unpaid", onSuspend: OnSuspend.STOP };
  shown({ status: OrganizationStatus.SUSPENDED, suspension } as Partial<Organization>, reach([]));
  expect(await screen.findByText(/the invoice of October is unpaid/)).toBeTruthy();
  expect(screen.getByText(/held stopped until it is resumed/)).toBeTruthy();
  expect(screen.getByText(/stay readable/)).toBeTruthy();
});

test("a caller holding the suspension above suspends with the workloads stopped", async () => {
  const asked: SuspendOrganizationRequest[] = [];
  shown({}, reach(["org.suspend"], asked));
  const reason = await screen.findByRole("textbox", {
    name: "Why, as its tenant will be shown it",
  });
  fireEvent.change(reason, { target: { value: "unpaid" } });
  fireEvent.click(screen.getByRole("checkbox", { name: "Hold its workloads stopped as well" }));
  fireEvent.click(screen.getByRole("button", { name: "Suspend" }));
  expect(await screen.findByRole("heading", { name: "Suspended" })).toBeTruthy();
  expect(asked.map((r) => [r.organization, r.reason, r.onSuspend])).toEqual([
    ["org_c", "unpaid", OnSuspend.STOP],
  ]);
});

test("no lever is offered to a caller holding nothing above, nor through a share link", async () => {
  shown({}, reach(["org.read"]));
  shown({}, reach(["org.suspend"], [], true));
  // Both read their grants and the organizations above before deciding; neither offers it.
  await new Promise((settle) => setTimeout(settle, 50));
  expect(screen.queryByRole("button", { name: "Suspend" })).toBeNull();
});
