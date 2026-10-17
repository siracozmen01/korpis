// SPDX-License-Identifier: AGPL-3.0-or-later

import { createClient, createRouterTransport } from "@connectrpc/connect";
import { render, screen } from "@testing-library/react";
import { expect, test } from "vitest";
import { ServerInfoService } from "../gen/korpis/v1/api_pb";
import { Brand, Themed, useWorn } from "./useTheme";

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
    <Themed source={{ kind: "sign-in", info: createClient(ServerInfoService, transport) }}>
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
