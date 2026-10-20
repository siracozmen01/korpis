// SPDX-License-Identifier: AGPL-3.0-or-later

import { createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen } from "@testing-library/react";
import { clientsOver, ReachProvider } from "../clients";
import {
  type CreateIdentityBindingRequest,
  GrantService,
  IdentityService,
  OrganizationService,
  type SetDelegationCeilingRequest,
} from "../gen/korpis/v1/api_pb";
import { BindingState, GrantState } from "../gen/korpis/v1/authority_pb";
import { Bindings } from "./Bindings";
import { Ceiling } from "./Ceiling";

const noRelay = () => Promise.reject(new Error("no relay in this test"));

/** The admins set at the reseller's provider, confirmed, asked every 30 minutes with 10 of grace. */
const admins = {
  id: "idb_a",
  organization: "org_r",
  provider: "corp",
  subject: {
    provider: "org_r/corp",
    selector: { case: "set" as const, value: { realm: "groups", memberOf: "admins" } },
  },
  state: BindingState.VERIFIED,
  lastVerifiedAt: { seconds: 1_700_000_000n },
  reverifyInterval: { seconds: 1800n },
  constraints: { grace: { seconds: 600n }, requireMembership: [] },
  version: 1n,
};

interface Seen {
  made: CreateIdentityBindingRequest[];
  asked: string[];
  ceilings: SetDelegationCeilingRequest[];
}

/** A reach whose caller holds `actions` on the reseller, `org_r`. */
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
    service(IdentityService, {
      listIdentityBindings: () => ({ bindings: [admins] }),
      createIdentityBinding: (r) => {
        seen.made.push(r);
        return { binding: { ...admins, id: "idb_n" } };
      },
      verifyIdentityBinding: (r) => {
        seen.asked.push(r.id);
        return { binding: admins, unanswered: "provider.deadline" };
      },
    });
    service(OrganizationService, {
      getOrganization: (r) => ({ organization: { id: r.id, name: r.id } }),
      getDelegationCeiling: () => {
        const own = seen.ceilings.at(-1)?.ceiling;
        return {
          ceiling: { effective: own ?? { seconds: 7n * 86_400n }, ...(own ? { own } : {}) },
        };
      },
      setDelegationCeiling: (r) => {
        seen.ceilings.push(r);
        return { ceiling: { effective: { seconds: 86_400n }, own: { seconds: 86_400n } } };
      },
    });
  });
  return { clients: clientsOver(transport), base: "", metered: false, relay: noRelay };
}

const none = (): Seen => ({ made: [], asked: [], ceilings: [] });

test("a binding is shown with its interval and grace together, and asked about now", async () => {
  const seen = none();
  render(
    <ReachProvider value={reach(["identity.binding.write"], seen)}>
      <Bindings organization="org_r" />
    </ReachProvider>,
  );
  expect(await screen.findByText("set:groups/admins")).toBeTruthy();
  expect(screen.getByText(/at most 40 minutes past its last confirmation/)).toBeTruthy();
  fireEvent.click(await screen.findByRole("button", { name: "Ask its provider now" }));
  expect((await screen.findByRole("alert")).textContent).toContain("provider.deadline");
  expect(seen.asked).toEqual(["idb_a"]);
});

test("only identity.binding.write binds, and a set is sent at the organization's provider", async () => {
  const seen = none();
  const { unmount } = render(
    <ReachProvider value={reach([], seen)}>
      <Bindings organization="org_r" />
    </ReachProvider>,
  );
  await screen.findByText("set:groups/admins");
  expect(screen.queryByRole("button", { name: "Bind a person or a set" })).toBeNull();
  unmount();
  render(
    <ReachProvider value={reach(["identity.binding.write"], seen)}>
      <Bindings organization="org_r" />
    </ReachProvider>,
  );
  fireEvent.click(await screen.findByRole("button", { name: "Bind a person or a set" }));
  const field = (name: string, value: string) => {
    fireEvent.change(screen.getByRole("textbox", { name }), { target: { value } });
  };
  field("Identity provider", "corp");
  field("Realm: the claim or guild naming the set", "groups");
  field("The set's name there", "ops");
  fireEvent.click(screen.getByRole("button", { name: "Save the binding" }));
  await screen.findByRole("button", { name: "Bind a person or a set" });
  const [made] = seen.made;
  expect(made?.organization).toBe("org_r");
  expect(made?.subject?.provider).toBe("org_r/corp");
  expect(made?.terms?.reverifyInterval?.seconds).toBe(900n);
});

test("the ceiling is shown, and lowered by whoever holds org.write there", async () => {
  const seen = none();
  render(
    <ReachProvider value={reach(["org.write"], seen)}>
      <Ceiling organization="org_r" />
    </ReachProvider>,
  );
  expect(await screen.findByText(/at most 7 days from its issue here/)).toBeTruthy();
  fireEvent.change(
    await screen.findByRole("textbox", { name: "Days a grant passed on beneath a set lasts" }),
    { target: { value: "1" } },
  );
  fireEvent.click(screen.getByRole("button", { name: "Set it" }));
  expect(await screen.findByText(/This organization sets 1 day of its own/)).toBeTruthy();
  expect(seen.ceilings[0]?.ceiling?.seconds).toBe(86_400n);
});
