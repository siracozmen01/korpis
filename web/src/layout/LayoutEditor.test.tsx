// SPDX-License-Identifier: AGPL-3.0-or-later

import { Code, ConnectError, createClient, createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { expect, test } from "vitest";
import { clientsOver, ReachProvider } from "../clients";
import { OrganizationService, type SetLayoutRequest } from "../gen/korpis/v1/api_pb";
import { RelayService } from "../gen/korpis/v1/relay_pb";
import type { MessageInitShape } from "@bufbuild/protobuf";
import type { LayoutSchema } from "../gen/korpis/v1/tenancy_pb";
import { LayoutEditor } from "./LayoutEditor";

const acme: MessageInitShape<typeof LayoutSchema> = {
  name: "Acme",
  items: [
    { kind: { case: "places", value: {} } },
    { kind: { case: "notice", value: { heading: "Hello", text: "" } } },
  ],
};

/** The editor of `org_1`, whose layout is set on `setOn`; what was saved. */
function setup(setOn: string, refuse = false) {
  const saved: SetLayoutRequest[] = [];
  const transport = createRouterTransport(({ service }) => {
    service(OrganizationService, {
      getLayout: () => ({
        governing: {
          setOn,
          ownVersion: setOn === "org_1" ? 2n : 0n,
          layout: acme,
        },
      }),
      setLayout: (r) => {
        if (refuse) throw new ConnectError("missing permission org.write", Code.PermissionDenied);
        saved.push(r);
        return { governing: {} };
      },
    });
  });
  render(
    <ReachProvider
      value={{
        clients: clientsOver(transport),
        base: "",
        metered: false,
        relay: () => Promise.resolve(createClient(RelayService, transport)),
      }}
    >
      <LayoutEditor id="org_1" />
    </ReachProvider>,
  );
  return saved;
}

const press = (name: string) => {
  fireEvent.click(screen.getByRole("button", { name }));
};

test("an organization's own landing is rearranged and saved against the version it was read at", async () => {
  const saved = setup("org_1");
  const heading = await screen.findByLabelText("Heading of the landing screen");
  expect((heading as HTMLInputElement).value).toBe("Acme");
  expect(screen.queryByRole("button", { name: "Remove: What the viewer can reach" })).toBeNull();
  fireEvent.change(screen.getByLabelText("Link label"), { target: { value: "Support" } });
  fireEvent.change(screen.getByLabelText("Link address (https://…)"), {
    target: { value: "https://help.acme.example" },
  });
  press("Add the link");
  press("Move up: A link: Support (https://help.acme.example)");
  press("Remove: A notice: Hello");
  press("Add the quota");
  expect(screen.queryByRole("button", { name: "Add the quota" })).toBeNull();
  press("Save the landing screen");
  await waitFor(() => {
    expect(saved).toHaveLength(1);
  });
  expect(saved[0]?.mutation?.expectedVersion).toBe(2n);
  expect(saved[0]?.layout?.items.map((i) => i.kind.case)).toEqual(["places", "link", "quota"]);
  expect(
    screen.getByRole("button", { name: "Remove this organization's landing screen" }),
  ).toBeTruthy();
});

test("an inherited landing starts a draft of what is reached alone, and a refusal says why", async () => {
  setup("org_p", true);
  await screen.findByLabelText("Heading of the landing screen");
  expect(screen.getByText("What the viewer can reach")).toBeTruthy();
  expect(screen.queryByText("A notice: Hello")).toBeNull();
  expect(
    screen.queryByRole("button", { name: "Remove this organization's landing screen" }),
  ).toBeNull();
  press("Save the landing screen");
  expect(await screen.findByRole("status")).toBeTruthy();
});
