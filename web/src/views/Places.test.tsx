// SPDX-License-Identifier: AGPL-3.0-or-later

import { Code, ConnectError, createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen } from "@testing-library/react";
import { clientsOver, ReachProvider } from "../clients";
import {
  GrantService,
  OrganizationService,
  ProjectService,
  WorkloadService,
} from "../gen/korpis/v1/api_pb";
import { GrantState } from "../gen/korpis/v1/authority_pb";
import { Home, ProjectPage } from "./Places";

const noRelay = () => Promise.reject(new Error("no relay in this test"));

const issued = GrantState.ISSUED;

function reach() {
  const transport = createRouterTransport(({ service }) => {
    service(GrantService, {
      listGrants: (r) =>
        r.page?.pageToken === ""
          ? {
              grants: [
                { scope: { kind: { case: "organization", value: "org_a" } }, state: issued },
                { scope: { kind: { case: "project", value: "prj_gone" } }, state: issued },
              ],
              page: { nextPageToken: "2" },
            }
          : { grants: [{ scope: { kind: { case: "workload", value: "wl_b" } }, state: issued }] },
    });
    service(OrganizationService, {
      getOrganization: (r) => ({ organization: { id: r.id, name: "Acme" } }),
    });
    service(ProjectService, {
      getProject: (r) => {
        if (r.id === "prj_gone") throw new ConnectError("not found", Code.NotFound);
        return { project: { id: r.id, name: "Games" } };
      },
    });
    service(WorkloadService, {
      getWorkload: (r) => ({ status: { workload: { id: r.id, name: "mc-survival" } } }),
      listWorkloads: (r) =>
        r.page?.pageToken === ""
          ? {
              workloads: [{ workload: { id: "wl_1", name: "first" } }],
              page: { nextPageToken: "n" },
            }
          : { workloads: [{ workload: { id: "wl_2", name: "second" } }] },
    });
  });
  return { clients: clientsOver(transport), base: "", metered: false, relay: noRelay };
}

test("the navigation starts at what every held grant opens, and leaves out what cannot be read", async () => {
  render(
    <ReachProvider value={reach()}>
      <Home />
    </ReachProvider>,
  );
  expect(await screen.findByRole("link", { name: "Acme" })).toHaveProperty(
    "href",
    `${window.location.origin}/o/org_a`,
  );
  expect(await screen.findByRole("link", { name: "mc-survival" })).toBeTruthy();
  expect(screen.queryByText("prj_gone")).toBeNull();
  expect(screen.queryByText(/locked/i)).toBeNull();
});

test("a list with more than one page says so and reads the next when asked", async () => {
  render(
    <ReachProvider value={reach()}>
      <ProjectPage id="prj_1" />
    </ReachProvider>,
  );
  expect(await screen.findByRole("heading", { name: "Games" })).toBeTruthy();
  expect(await screen.findByRole("link", { name: "first" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Show more" }));
  expect(await screen.findByRole("link", { name: "second" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Show more" })).toBeNull();
});
