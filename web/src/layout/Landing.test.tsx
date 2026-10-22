// SPDX-License-Identifier: AGPL-3.0-or-later

import { createRouterTransport } from "@connectrpc/connect";
import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import { clientsOver, ReachProvider } from "../clients";
import { GrantService, OrganizationService } from "../gen/korpis/v1/api_pb";
import type { MessageInitShape } from "@bufbuild/protobuf";
import type { LayoutSchema } from "../gen/korpis/v1/tenancy_pb";
import { Landing } from "./Landing";

const noRelay = () => Promise.reject(new Error("no relay in this test"));

type Init = MessageInitShape<typeof LayoutSchema>;

const pinecone: Init = {
  name: "Pinecone",
  items: [
    {
      kind: {
        case: "notice",
        value: { heading: "Maintenance tonight", text: "From 22:00.\nAbout an hour." },
      },
    },
    { kind: { case: "places", value: {} } },
    {
      kind: {
        case: "link",
        value: { label: "Support", url: "https://help.pinecone.example/desk" },
      },
    },
  ],
};

/** A viewer who reads `orgs`, whose landing the control plane answers with `layout`; what was
 * asked. */
function landing(orgs: { id: string; parent?: string }[], layout?: Init) {
  const asked: string[] = [];
  const transport = createRouterTransport(({ service }) => {
    service(GrantService, { listGrants: () => ({ grants: [] }) });
    service(OrganizationService, {
      listOrganizations: () => ({ organizations: orgs.map((o) => ({ ...o, name: o.id })) }),
      getLayout: () => {
        asked.push("an organization's");
        return { governing: {} };
      },
      getLandingLayout: () => {
        asked.push("landing");
        return { governing: { layout, setOn: "org_northwind" } };
      },
    });
  });
  render(
    <ReachProvider
      value={{ clients: clientsOver(transport), base: "", metered: false, relay: noRelay }}
    >
      <Landing />
    </ReachProvider>,
  );
  return asked;
}

test("a provider's landing shows its notice as text, its link with its host, and what is reached", async () => {
  const asked = landing([], pinecone);
  expect(await screen.findByRole("heading", { name: "Maintenance tonight" })).toBeTruthy();
  expect(screen.getByText("Pinecone")).toBeTruthy();
  expect(screen.getByText(/From 22:00\.\s+About an hour\./)).toBeTruthy();
  const support = screen.getByRole("link", { name: "Support" });
  expect(support.getAttribute("rel")).toBe("noopener noreferrer");
  expect(screen.getByText("(goes to help.pinecone.example)")).toBeTruthy();
  expect(await screen.findByRole("heading", { name: "What you can reach" })).toBeTruthy();
  expect(asked, "the control plane's choice, though they read no organization").toEqual([
    "landing",
  ]);
});

test("with no layout, the landing is the panel's own", async () => {
  landing([{ id: "org_a" }, { id: "org_b" }]);
  expect(await screen.findByRole("heading", { name: "What you can reach" })).toBeTruthy();
  expect(screen.queryByRole("heading", { name: "Maintenance tonight" })).toBeNull();
});
