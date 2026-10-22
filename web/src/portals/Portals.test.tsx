// SPDX-License-Identifier: AGPL-3.0-or-later

import { createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen } from "@testing-library/react";
import { clientsOver, ReachProvider } from "../clients";
import {
  type CreatePortalRequest,
  GrantService,
  OrganizationService,
  PortalService,
} from "../gen/korpis/v1/api_pb";
import { CertificateSource, CertificateState } from "../gen/korpis/v1/attached_pb";
import { GrantState } from "../gen/korpis/v1/authority_pb";
import { Portals } from "./Portals";

const noRelay = () => Promise.reject(new Error("no relay in this test"));

const panel = {
  id: "ptl_p",
  organization: "org_r",
  name: "panel.pinecone.example",
  certificate: "crt_own",
  certificateSource: CertificateSource.ACME,
  certificateState: CertificateState.ISSUED,
};

interface Seen {
  made: CreatePortalRequest[];
  deleted: string[];
}

/** A reach whose caller holds `actions` on the reseller, `org_r`, which has one portal. */
function reach(actions: string[], seen: Seen) {
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
    });
    service(PortalService, {
      listPortals: () => ({ portals: [panel] }),
      getPortal: () => ({ portal: panel, passkeys: 3n, accountsOnlyHere: 1n }),
      createPortal: (r) => {
        seen.made.push(r);
        return { portal: { ...panel, id: "ptl_n", name: r.name } };
      },
      deletePortal: (r) => {
        seen.deleted.push(r.id);
        return {};
      },
    });
  });
  return { clients: clientsOver(transport), base: "", metered: false, relay: noRelay };
}

const none = (): Seen => ({ made: [], deleted: [] });

test("a portal is shown with how it is served, and made only under portal.write", async () => {
  const seen = none();
  const { unmount } = render(
    <ReachProvider value={reach(["portal.read"], seen)}>
      <Portals organization="org_r" />
    </ReachProvider>,
  );
  expect(await screen.findByText("panel.pinecone.example")).toBeTruthy();
  expect(screen.getByText("Served with its own certificate.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Make a portal" })).toBeNull();
  unmount();
  render(
    <ReachProvider value={reach(["portal.write"], seen)}>
      <Portals organization="org_r" />
    </ReachProvider>,
  );
  fireEvent.click(await screen.findByRole("button", { name: "Make a portal" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Name, one this organization holds" }), {
    target: { value: "Shop.Pinecone.Example" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Make the portal" }));
  await screen.findByRole("button", { name: "Make a portal" });
  expect(seen.made[0]?.name).toBe("shop.pinecone.example");
  expect(seen.made[0]?.certificate, "its own certificate").toBe("");
});

test("deleting a portal says first what it removes", async () => {
  const seen = none();
  render(
    <ReachProvider value={reach(["portal.write"], seen)}>
      <Portals organization="org_r" />
    </ReachProvider>,
  );
  fireEvent.click(await screen.findByRole("button", { name: "Delete…" }));
  expect(await screen.findByText(/removes the 3 passkeys registered there/)).toBeTruthy();
  expect(screen.getByText(/1 account signs in nowhere else/)).toBeTruthy();
  expect(seen.deleted).toEqual([]);
  fireEvent.click(screen.getByRole("button", { name: "Delete the portal" }));
  await screen.findByRole("button", { name: "Delete…" });
  expect(seen.deleted).toEqual(["ptl_p"]);
});
