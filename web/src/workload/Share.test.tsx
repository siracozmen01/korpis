// SPDX-License-Identifier: AGPL-3.0-or-later

import { timestampMs } from "@bufbuild/protobuf/wkt";
import { createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen } from "@testing-library/react";
import { clientsOver, ReachProvider } from "../clients";
import { GrantService, type IssueGrantRequest } from "../gen/korpis/v1/api_pb";
import { GrantState } from "../gen/korpis/v1/authority_pb";
import { Grants } from "./Grants";
import { Share } from "./Share";

const noRelay = () => Promise.reject(new Error("no relay in this test"));

function reach(seen: { issued?: IssueGrantRequest; revoked?: string }) {
  const transport = createRouterTransport(({ service }) => {
    service(GrantService, {
      issueGrant: (r) => {
        seen.issued = r;
        return { linkToken: "korpislk_abc" };
      },
      listGrants: () => ({
        grants: [
          {
            id: "grt_1",
            subject: { kind: { case: "link", value: "grt_1" } },
            actions: ["workload.restart"],
            state: GrantState.ISSUED,
            label: "for the moderator",
          },
        ],
      }),
      revokeGrant: (r) => {
        seen.revoked = r.grant;
        return { cascaded: 2n };
      },
    });
  });
  return { clients: clientsOver(transport), base: "", metered: false, relay: noRelay };
}

test("a link is issued for one workload, with an expiry and a use cap, and shown once", async () => {
  const seen: { issued?: IssueGrantRequest } = {};
  render(
    <ReachProvider value={reach(seen)}>
      <Share id="wl_1" />
    </ReachProvider>,
  );
  expect(screen.getByText(/Opening its screen is one use/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Make the link" }));
  expect(await screen.findByText(`${window.location.origin}/g/#korpislk_abc`)).toBeTruthy();
  const r = seen.issued;
  expect(r?.linkForm).toBe(true);
  expect(r?.scope?.kind).toEqual({ case: "workload", value: "wl_1" });
  expect(r?.actions).toEqual(["workload.read", "workload.console.read"]);
  expect(r?.conditions?.maxUses).toBe(3n);
  const expires = r?.conditions?.expiresAt;
  const hours = expires === undefined ? 0 : (timestampMs(expires) - Date.now()) / 3_600_000;
  expect(Math.round(hours)).toBe(24);
});

test("a grant on the workload is listed and revoked, with what it took with it", async () => {
  const seen: { revoked?: string } = {};
  render(
    <ReachProvider value={reach(seen)}>
      <Grants id="wl_1" />
    </ReachProvider>,
  );
  expect(await screen.findByText(/whoever holds a share link: workload.restart/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Revoke" }));
  expect(await screen.findByText("Revoked, with 2 grants attenuated from it.")).toBeTruthy();
  expect(seen.revoked).toBe("grt_1");
  expect(screen.queryByRole("button", { name: "Revoke" })).toBeNull();
});
