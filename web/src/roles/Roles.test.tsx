// SPDX-License-Identifier: AGPL-3.0-or-later

import { createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen } from "@testing-library/react";
import { clientsOver, ReachProvider } from "../clients";
import {
  type CreateGrantTemplateRequest,
  GrantService,
  type IssueGrantRequest,
  OrganizationService,
} from "../gen/korpis/v1/api_pb";
import { GrantState } from "../gen/korpis/v1/authority_pb";
import { ObjectKind } from "../gen/korpis/v1/common_pb";
import { Roles } from "./Roles";

const noRelay = () => Promise.reject(new Error("no relay in this test"));

/** The reseller's Moderator, given on projects, restarting and reading consoles. */
const moderator = {
  id: "tpl_m",
  organization: "org_r",
  name: "Moderator",
  version: 2n,
  actions: ["workload.console.read", "workload.restart"],
  scopePattern: { kind: ObjectKind.PROJECT },
  delegation: { allowed: true, maxDepth: 1, actions: ["workload.restart"] },
  conditions: { lifetime: { seconds: 86_400n } },
};

/** A reach whose caller holds `actions` on the reseller, `org_r`. */
function reach(
  actions: string[],
  seen: { issued: IssueGrantRequest[]; made: CreateGrantTemplateRequest[] },
) {
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
      listGrantTemplates: () => ({ templates: [moderator] }),
      issueGrant: (r) => {
        seen.issued.push(r);
        return { invitation: "korpisiv_abc" };
      },
      createGrantTemplate: (r) => {
        seen.made.push(r);
        return { template: { ...moderator, id: "tpl_n", name: r.template?.name ?? "" } };
      },
    });
    service(OrganizationService, {
      getOrganization: (r) => ({ organization: { id: r.id, name: r.id } }),
    });
  });
  return { clients: clientsOver(transport), base: "", metered: false, relay: noRelay };
}

test("a role offered at a project is given to someone invited, who is sent a link once", async () => {
  const seen = { issued: [], made: [] };
  render(
    <ReachProvider value={reach([], seen)}>
      <Roles place={{ case: "project", id: "prj_s" }} />
    </ReachProvider>,
  );
  expect(await screen.findByText(/Allows workload.console.read, workload.restart/)).toBeTruthy();
  expect(screen.getByText(/from above/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Give this role" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Their name, as they will be shown" }), {
    target: { value: "Ada" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Give Moderator" }));
  expect((await screen.findByRole("status")).textContent).toContain("/invitation#korpisiv_abc");
  const [asked] = seen.issued as IssueGrantRequest[];
  expect(asked?.template).toMatchObject({ template: "tpl_m", version: 2n });
  expect(asked?.scope?.kind).toEqual({ case: "project", value: "prj_s" });
  expect(asked?.toNewUser?.displayName).toBe("Ada");
  expect(asked?.actions).toEqual([]);
});

test("a role is not given where its kind is not the place's, and only template.write makes one", async () => {
  const seen = { issued: [], made: [] };
  render(
    <ReachProvider value={reach(["template.write"], seen)}>
      <Roles place={{ case: "organization", id: "org_r" }} />
    </ReachProvider>,
  );
  await screen.findByText(/Allows/);
  expect(screen.queryByRole("button", { name: "Give this role" })).toBeNull();
  fireEvent.click(await screen.findByRole("button", { name: "A new role" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Name" }), { target: { value: "Auditor" } });
  fireEvent.change(
    screen.getByRole("textbox", { name: "Actions it allows, separated by commas" }),
    {
      target: { value: "workload.read" },
    },
  );
  fireEvent.click(screen.getByRole("button", { name: "Save the role" }));
  await screen.findByRole("button", { name: "A new role" });
  const [made] = seen.made as CreateGrantTemplateRequest[];
  expect(made?.organization).toBe("org_r");
  expect(made?.template?.actions).toEqual(["workload.read"]);
  expect(made?.template?.delegation?.allowed).toBe(false);
});
