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

test("a signed-in viewer wears the theme of whoever the control plane says provides their access", async () => {
  const asked: string[] = [];
  const transport = createRouterTransport(({ service }) => {
    service(OrganizationService, {
      getLandingTheme: () => {
        asked.push("landing");
        return { governing: { setOn: "org_northwind", theme: { productName: "Northwind Games" } } };
      },
      getTheme: () => {
        asked.push("an organization's");
        return { governing: {} };
      },
    });
  });
  render(
    <Themed source={{ kind: "viewer", clients: clientsOver(transport) }} stylesheet>
      <Brand />
    </Themed>,
  );
  expect(await screen.findByText("Northwind Games")).toBeTruthy();
  expect(asked, "though they may read none of its organizations").toEqual(["landing"]);
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
