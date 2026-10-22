// SPDX-License-Identifier: AGPL-3.0-or-later

import { create } from "@bufbuild/protobuf";
import { createClient, createRouterTransport } from "@connectrpc/connect";
import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import { clientsOver } from "../clients";
import { OrganizationService, ServerInfoService } from "../gen/korpis/v1/api_pb";
import { OrganizationSchema } from "../gen/korpis/v1/tenancy_pb";
import { Brand, Themed, topOf, useWorn } from "./useTheme";

function SignInText() {
  return <p>{useWorn()?.signInText}</p>;
}

test("before anyone signs in, the page wears the installation root's theme", async () => {
  const transport = createRouterTransport(({ service }) => {
    service(ServerInfoService, {
      getSignInTheme: () => ({
        governing: {
          setOn: "org_root",
          theme: {
            productName: "Acme Hosting",
            signInText: "Sign in to manage your servers.",
            logo: { mediaType: "image/svg+xml", data: new TextEncoder().encode("<svg/>") },
          },
        },
      }),
    });
  });
  render(
    <Themed
      source={{ kind: "sign-in", info: createClient(ServerInfoService, transport) }}
      stylesheet
    >
      <Brand />
      <SignInText />
    </Themed>,
  );
  expect(await screen.findByText("Acme Hosting")).toBeTruthy();
  expect(screen.getByText("Sign in to manage your servers.")).toBeTruthy();
  expect(document.querySelector("img")?.getAttribute("src")).toMatch(
    /^data:image\/svg\+xml;base64,/,
  );
  expect(document.title).toBe("Acme Hosting");
});

const org = (id: string, parent?: string) => create(OrganizationSchema, { id, name: id, parent });

test("the top of what a viewer reads is the one organization whose parent they cannot read", () => {
  expect(topOf([org("org_c", "org_r"), org("org_r", "org_host")])?.id).toBe("org_r");
  expect(topOf([org("org_root"), org("org_c", "org_root")])?.id).toBe("org_root");
  expect(topOf([org("org_a", "org_x"), org("org_b", "org_y")]), "two providers").toBeUndefined();
  expect(topOf([]), "none").toBeUndefined();
});

/** A viewer reading `orgs`, each organization's theme named after it, and the sign-in theme's,
 * at `portal` where one is named. */
function viewer(orgs: ReturnType<typeof org>[], portal = "") {
  const asked: string[] = [];
  const transport = createRouterTransport(({ service }) => {
    service(OrganizationService, {
      listOrganizations: () => ({ organizations: orgs }),
      getTheme: (r) => {
        const of = r.of?.kind.case === "organization" ? r.of.kind.value : "";
        asked.push(of);
        return { governing: { setOn: of, theme: { productName: `Theme of ${of}` } } };
      },
    });
    service(ServerInfoService, {
      getServerInfo: () => ({ portal, portalOrganization: portal === "" ? "" : "org_reseller" }),
      getSignInTheme: () => ({
        governing: {
          theme: { productName: portal === "" ? "The installation's" : "The portal's" },
        },
      }),
    });
  });
  const source = {
    kind: "viewer" as const,
    clients: clientsOver(transport),
    info: createClient(ServerInfoService, transport),
  };
  render(
    <Themed source={source} stylesheet>
      <Brand />
    </Themed>,
  );
  return asked;
}

test("a signed-in viewer wears their provider's theme, never a deeper tenant's", async () => {
  const asked = viewer([org("org_customer", "org_host"), org("org_reseller", "org_customer")]);
  expect(await screen.findByText("Theme of org_customer")).toBeTruthy();
  expect(asked).toEqual(["org_customer"]);
});

test("a viewer with no single provider wears the installation's theme", async () => {
  const asked = viewer([org("org_a", "org_x"), org("org_b", "org_y")]);
  expect(await screen.findByText("The installation's")).toBeTruthy();
  expect(asked).toEqual([]);
});

test("at a portal a viewer wears the portal's organization's theme, whatever they read", async () => {
  const asked = viewer([org("org_customer", "org_host")], "ptl_pinecone");
  expect(await screen.findByText("The portal's")).toBeTruthy();
  expect(asked, "no organization's theme read in its place").toEqual([]);
});

test("a page leaving every theme leaves the panel's own on", async () => {
  const transport = createRouterTransport(({ service }) => {
    service(ServerInfoService, {
      getSignInTheme: () => ({ governing: { theme: { productName: "Acme Hosting" } } }),
    });
  });
  const shown = render(
    <Themed
      source={{ kind: "sign-in", info: createClient(ServerInfoService, transport) }}
      stylesheet
    >
      <Brand />
    </Themed>,
  );
  expect(await screen.findByText("Acme Hosting")).toBeTruthy();
  shown.unmount();
  expect(document.title).toBe("Korpis");
});
